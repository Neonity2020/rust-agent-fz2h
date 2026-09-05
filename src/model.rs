use crate::tools;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::time::Duration;

const ENDPOINT: &str = "https://opencode.ai/zen/go/v1/chat/completions";
pub(crate) const MODEL: &str = "omen-alpha";

pub(crate) fn create_client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("rust-agent/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()
        .context("创建 HTTP 客户端失败")
}

pub(crate) fn answer_text(message: &Value) -> Result<String> {
    let content = &message["content"];
    let text = if let Some(text) = content.as_str() {
        text.to_owned()
    } else if let Some(parts) = content.as_array() {
        parts
            .iter()
            .filter(|part| part["type"] == "text")
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };
    if text.trim().is_empty() {
        bail!("接口未返回非空回答文本");
    }
    Ok(text)
}

fn parse_response(body: &Value, allow_tools: bool) -> Result<Value> {
    let choice = &body["choices"][0];
    let message = &choice["message"];
    if !message.is_object() {
        bail!("接口缺少 assistant 消息");
    }
    let reason = choice["finish_reason"].as_str().unwrap_or("unknown");
    let calls = message["tool_calls"].as_array().map_or(0, Vec::len);
    if reason == "length" {
        bail!("模型输出被截断（finish_reason=length），请缩小本轮任务或读取范围");
    }
    if calls > 0 {
        if allow_tools {
            return Ok(message.clone());
        }
        bail!(
            "模型在总结阶段仍返回 {calls} 个工具调用（finish_reason={reason}）；已达到探索上限，不再执行工具，请缩小任务后重试"
        );
    }
    if answer_text(message).is_err() {
        let reasoning = ["reasoning_content", "reasoning"]
            .iter()
            .any(|key| message[*key].as_str().is_some_and(|text| !text.is_empty()));
        let refusal = message["refusal"]
            .as_str()
            .is_some_and(|text| !text.is_empty());
        bail!(
            "模型未返回最终回答（finish_reason={reason}, 含推理内容={reasoning}, 含拒绝信息={refusal}）。请重试或缩小任务；推理内容不会作为答案输出"
        );
    }
    Ok(message.clone())
}

pub(crate) fn request(
    client: &Client,
    api_key: &str,
    session: &str,
    messages: &[Value],
    allow_tools: bool,
) -> Result<Value> {
    eprintln!("正在请求 {MODEL}…");
    let mut request_messages = messages.to_vec();
    if !allow_tools {
        request_messages.push(json!({"role":"user","content":"本轮工具执行已结束。请立即根据现有工具结果，用中文给出最终回答，不再调用工具。资料不足或工具失败时，请说明已确认的事实及缺少的信息。"}));
    }
    let payload = json!({
        "model": MODEL,
        "messages": request_messages,
        "tools": tools::definitions(),
        "tool_choice": if allow_tools { "auto" } else { "none" },
        "stream": false
    });
    let bytes = serde_json::to_vec(&payload)?;
    if bytes.len() > 512 * 1024 {
        bail!("请求超过 512 KiB，未发送。请 /save 后 /clear，或缩小工具读取范围");
    }
    let started = std::time::Instant::now();
    let response = client
        .post(ENDPOINT)
        .bearer_auth(api_key)
        .header("x-opencode-session", session)
        .header("content-type", "application/json")
        .body(bytes)
        .send()
        .context("请求失败，请检查网络；请求最长等待 180 秒")?;

    let status = response.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => "请检查 API Key、Go 订阅和模型访问权限",
            429 => "请求受限，请检查 Go 可用额度或稍后重试",
            500..=599 => "模型服务暂时异常，请稍后重试",
            _ => "请检查接口参数和模型可用性",
        };
        bail!("OpenCode Go 返回 HTTP {status}：{hint}");
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    response
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .context("读取模型响应失败")?;
    if bytes.len() > 2 * 1024 * 1024 {
        bail!("模型响应超过 2 MiB，停止读取");
    }
    eprintln!(
        "模型请求完成，用时 {:.1} 秒。",
        started.elapsed().as_secs_f64()
    );
    let body: Value = serde_json::from_slice(&bytes).context("接口返回的内容不是有效 JSON")?;
    parse_response(&body, allow_tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_and_tool_responses() -> Result<()> {
        for content in [json!("回答"), json!([{"type":"text","text":"回答"}])] {
            let response =
                json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]});
            assert_eq!(answer_text(&parse_response(&response, false)?)?, "回答");
        }
        let response = json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":"1"}]}}]});
        assert!(parse_response(&response, true).is_ok());
        assert!(
            parse_response(&response, false)
                .unwrap_err()
                .to_string()
                .contains("仍返回")
        );
        Ok(())
    }
    #[test]
    fn empty_and_truncated_responses_are_diagnostic() {
        for reason in ["stop", "length", "content_filter"] {
            let response = json!({"choices":[{"finish_reason":reason,"message":{"content":null,"reasoning_content":"private reasoning"}}]});
            let error = parse_response(&response, false).unwrap_err().to_string();
            assert!(error.contains(reason));
            assert!(!error.contains("private reasoning"));
        }
    }
}
