use crate::{
    tools,
    tui::{self, Ui},
};
use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};
use std::{
    env,
    io::{BufRead, BufReader, Read},
    sync::mpsc,
    time::{Duration, Instant},
};

const DEFAULT_ENDPOINT: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const DEFAULT_MODEL: &str = "omen-alpha";
const MAX_REQUEST_BYTES: usize = 512 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const TICK: Duration = Duration::from_millis(120);

// 默认走 OpenCode Go；环境变量可切换到其他兼容接口，便于接入本地或其他服务。
fn env_string(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn endpoint() -> String {
    env_string("RUST_AGENT_ENDPOINT").unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned())
}

pub(crate) fn model() -> String {
    env_string("RUST_AGENT_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_owned())
}

pub(crate) fn custom_endpoint() -> bool {
    env_string("RUST_AGENT_ENDPOINT").is_some()
}

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

// 收尾校验对流式与非流式共用，保证两条路径的失败提示一致。
fn finish_message(message: &Value, reason: &str, allow_tools: bool) -> Result<Value> {
    if reason == "length" {
        bail!("模型输出被截断（finish_reason=length），请缩小本轮任务或读取范围");
    }
    let calls = message["tool_calls"].as_array().map_or(0, Vec::len);
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

fn parse_response(body: &Value, allow_tools: bool) -> Result<Value> {
    let choice = &body["choices"][0];
    let message = &choice["message"];
    if !message.is_object() {
        bail!("接口缺少 assistant 消息");
    }
    let reason = choice["finish_reason"].as_str().unwrap_or("unknown");
    finish_message(message, reason, allow_tools)
}

/// SSE 分片累加器：正文逐片下发，工具调用按 index 拼回完整 JSON 参数。
#[derive(Default)]
struct StreamState {
    text: String,
    calls: Vec<PartialCall>,
    reason: String,
}

#[derive(Default, Clone)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamState {
    fn push(&mut self, chunk: &Value) {
        let Some(choice) = chunk["choices"]
            .as_array()
            .and_then(|choices| choices.first())
        else {
            return;
        };
        if let Some(reason) = choice["finish_reason"].as_str()
            && !reason.is_empty()
            && reason != "null"
        {
            self.reason = reason.to_owned();
        }
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str() {
            self.text.push_str(text);
        }
        let Some(calls) = delta["tool_calls"].as_array() else {
            return;
        };
        for call in calls {
            let index = call["index"].as_u64().unwrap_or(0) as usize;
            if self.calls.len() <= index {
                self.calls.resize(index + 1, PartialCall::default());
            }
            let slot = &mut self.calls[index];
            if let Some(id) = call["id"].as_str() {
                slot.id.push_str(id);
            }
            if let Some(name) = call["function"]["name"].as_str() {
                slot.name.push_str(name);
            }
            if let Some(arguments) = call["function"]["arguments"].as_str() {
                slot.arguments.push_str(arguments);
            }
        }
    }

    fn message(&self) -> Value {
        let calls: Vec<Value> = self
            .calls
            .iter()
            .filter(|call| !call.name.is_empty() || !call.arguments.is_empty())
            .enumerate()
            .map(|(index, call)| {
                json!({"id": if call.id.is_empty() { format!("call_{index}") } else { call.id.clone() },
                       "type": "function",
                       "function": {"name": call.name, "arguments": call.arguments}})
            })
            .collect();
        let mut message = json!({
            "role": "assistant",
            "content": if self.text.is_empty() { Value::Null } else { Value::String(self.text.clone()) }
        });
        if !calls.is_empty() {
            message["tool_calls"] = Value::Array(calls);
        }
        message
    }

    fn finish(self, allow_tools: bool) -> Result<Value> {
        let reason = if self.reason.is_empty() {
            "stop"
        } else {
            self.reason.as_str()
        };
        finish_message(&self.message(), reason, allow_tools)
    }
}

fn payload(messages: &[Value], allow_tools: bool, stream: bool) -> Result<Vec<u8>> {
    let mut request_messages = messages.to_vec();
    if !allow_tools {
        request_messages.push(json!({"role":"user","content":"本轮工具执行已结束。请立即根据现有工具结果，用中文给出最终回答，不再调用工具。资料不足或工具失败时，请说明已确认的事实及缺少的信息。"}));
    }
    let bytes = serde_json::to_vec(&json!({
        "model": model(),
        "messages": request_messages,
        "tools": tools::definitions(),
        "tool_choice": if allow_tools { "auto" } else { "none" },
        "stream": stream
    }))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        bail!("请求超过 512 KiB，未发送。请 /save 后 /clear，或缩小工具读取范围");
    }
    Ok(bytes)
}

fn post(
    client: &Client,
    endpoint: &str,
    body: &[u8],
    api_key: &str,
    session: &str,
) -> Result<Response> {
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .header("x-opencode-session", session)
        .header("content-type", "application/json")
        .body(body.to_vec())
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
    Ok(response)
}

enum Event {
    Delta(String),
    Done(Result<Value>),
}

/// 读取 SSE 分片并累加。正文实时下发，工具参数在收到 [DONE] 后才算完整。
fn consume_stream(
    response: Response,
    allow_tools: bool,
    tx: &mpsc::Sender<Event>,
) -> Result<Value> {
    let mut reader = BufReader::new(response.take((MAX_RESPONSE_BYTES + 1) as u64));
    let mut state = StreamState::default();
    let mut line = String::new();
    let mut sent = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue; // 注释行与空行
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            continue; // 非 JSON 心跳，忽略
        };
        if let Some(error) = chunk.get("error") {
            let reason = error["message"].as_str().unwrap_or("未知错误");
            bail!("接口返回错误：{reason}");
        }
        state.push(&chunk);
        if state.text.len() > sent {
            tx.send(Event::Delta(state.text[sent..].to_owned())).ok();
            sent = state.text.len();
        }
    }
    state.finish(allow_tools)
}

/// 在工作线程里跑完整个请求，主线程只等结果，这样 Ctrl+C 随时能收回控制权。
fn stream_request(
    client: &Client,
    endpoint: &str,
    body: &[u8],
    api_key: &str,
    session: &str,
    ui: &Ui,
    allow_tools: bool,
) -> Result<Value> {
    let (tx, rx) = mpsc::channel::<Event>();
    let worker = client.clone();
    let (endpoint, body, api_key, session) = (
        endpoint.to_owned(),
        body.to_vec(),
        api_key.to_owned(),
        session.to_owned(),
    );
    let sender = tx.clone();
    std::thread::spawn(move || {
        let result = post(&worker, &endpoint, &body, &api_key, &session)
            .and_then(|response| consume_stream(response, allow_tools, &sender));
        let _ = tx.send(Event::Done(result));
    });

    let started = Instant::now();
    ui.busy_start("等待模型响应");
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Event::Delta(text)) => {
                ui.busy_stop();
                ui.delta(&text);
            }
            Ok(Event::Done(result)) => {
                ui.busy_stop();
                return result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                ui.tick(&format!(
                    "等待模型响应 · {:.0}s",
                    started.elapsed().as_secs_f64()
                ));
                if tui::cancelled() {
                    ui.busy_stop();
                    ui.stream_end(true);
                    bail!("已取消本次请求");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                ui.busy_stop();
                bail!("模型连接意外中断");
            }
        }
    }
}

/// 非流式路径：适合重定向到文件或跑测试，行为与改动前一致。
fn blocking_request(
    client: &Client,
    endpoint: &str,
    body: &[u8],
    api_key: &str,
    session: &str,
    ui: &Ui,
    allow_tools: bool,
) -> Result<Value> {
    ui.note(&format!("正在请求 {}…", model()));
    let started = Instant::now();
    let response = post(client, endpoint, body, api_key, session)?;
    let mut bytes = Vec::new();
    response
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("读取模型响应失败")?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        bail!("模型响应超过 2 MiB，停止读取");
    }
    ui.note(&format!(
        "模型请求完成，用时 {:.1} 秒。",
        started.elapsed().as_secs_f64()
    ));
    let body: Value = serde_json::from_slice(&bytes).context("接口返回的内容不是有效 JSON")?;
    parse_response(&body, allow_tools)
}

pub(crate) fn request(
    client: &Client,
    api_key: &str,
    session: &str,
    messages: &[Value],
    allow_tools: bool,
    ui: &Ui,
) -> Result<Value> {
    tui::reset_cancel();
    let interactive = ui.is_interactive();
    let body = payload(messages, allow_tools, interactive)?;
    let endpoint = endpoint();
    let result = if interactive {
        stream_request(client, &endpoint, &body, api_key, session, ui, allow_tools)
    } else {
        blocking_request(client, &endpoint, &body, api_key, session, ui, allow_tools)
    };
    match &result {
        Ok(_) => ui.stream_end(false),
        Err(_) if tui::cancelled() => ui.stream_end(true),
        Err(_) => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::Ui;
    use std::{io::Write as _, net::TcpListener};

    /// 起一个只回固定响应的本地服务；prefix 为 Some 时只发前若干字节就挂住，
    /// 用来模拟"连接还在但迟迟不结束"的响应。
    fn mock_sse(body: &str, prefix: Option<usize>) -> Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let body = body.to_owned();
        std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while socket.read(&mut byte).unwrap_or(0) == 1 {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&head).to_string();
            let length: usize = text
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            let mut payload = vec![0u8; length];
            let _ = socket.read_exact(&mut payload);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n"
            );
            let _ = socket.write_all(head.as_bytes());
            match prefix {
                Some(count) => {
                    let _ = socket.write_all(&body.as_bytes()[..count]);
                    let _ = socket.flush();
                    for _ in 0..100 {
                        if socket.write_all(b"").is_err() || socket.flush().is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                None => {
                    let _ = socket.write_all(body.as_bytes());
                    let _ = socket.flush();
                }
            }
        });
        Ok(format!("http://{address}/v1/chat/completions"))
    }

    fn sse(chunks: &[&str]) -> String {
        chunks
            .iter()
            .map(|chunk| format!("data: {chunk}\n\n"))
            .collect()
    }

    #[test]
    fn streaming_request_renders_tokens_live_and_returns_the_message() -> Result<()> {
        let endpoint = mock_sse(
            &sse(&[
                r#"{"choices":[{"delta":{"content":"正在"}}]}"#,
                r#"{"choices":[{"delta":{"content":"流式输出"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                "[DONE]",
            ]),
            None,
        )?;
        let (ui, buffer) = Ui::capture();
        let client = create_client()?;
        let body = payload(&[json!({"role": "user", "content": "你好"})], true, true)?;
        let message = stream_request(&client, &endpoint, &body, "test-key", "sess", &ui, true)?;
        assert_eq!(message["content"], "正在流式输出");
        assert_eq!(message["role"], "assistant");
        // 正文在流结束前就已经写到终端上，而不是等整段收完。
        assert!(
            buffer.borrow().contains("正在流式输出"),
            "{}",
            buffer.borrow()
        );
        Ok(())
    }

    #[test]
    fn streaming_tool_call_is_reassembled_over_the_wire() -> Result<()> {
        let endpoint = mock_sse(
            &sse(&[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\""}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"src/main.rs\"}"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                "[DONE]",
            ]),
            None,
        )?;
        let (ui, _buffer) = Ui::capture();
        let client = create_client()?;
        let body = payload(&[json!({"role": "user", "content": "读文件"})], true, true)?;
        let message = stream_request(&client, &endpoint, &body, "test-key", "sess", &ui, true)?;
        let calls = message["tool_calls"].as_array().expect("工具调用");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["function"]["name"], "read_file");
        assert_eq!(
            calls[0]["function"]["arguments"],
            "{\"path\":\"src/main.rs\"}"
        );
        Ok(())
    }

    #[test]
    fn ctrl_c_stops_the_request_and_keeps_partial_output() -> Result<()> {
        let body_text = sse(&[r#"{"choices":[{"delta":{"content":"开头"}}]}"#]);
        let prefix = body_text.find("\n\n").expect("分片") + 2;
        let endpoint = mock_sse(&body_text, Some(prefix))?;
        let (ui, buffer) = Ui::capture();
        let client = create_client()?;
        let body = payload(&[json!({"role": "user", "content": "慢一点"})], true, true)?;
        let stopper = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(400));
            unsafe { libc::raise(libc::SIGINT) };
        });
        tui::install_interrupt_handler();
        let error = stream_request(&client, &endpoint, &body, "test-key", "sess", &ui, false)
            .expect_err("应当被取消");
        stopper.join().unwrap();
        tui::reset_cancel();
        assert!(error.to_string().contains("取消"), "{error}");
        assert!(
            buffer.borrow().contains("开头"),
            "已收到的内容要保留：{}",
            buffer.borrow()
        );
        Ok(())
    }

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

    #[test]
    fn stream_accumulates_text_in_order() -> Result<()> {
        let mut state = StreamState::default();
        for chunk in ["片段一", "片段二", "片段三"] {
            state.push(&json!({"choices":[{"delta":{"content":chunk}}]}));
        }
        state.reason = "stop".to_owned();
        let message = state.finish(false)?;
        assert_eq!(message["content"], "片段一片段二片段三");
        Ok(())
    }

    #[test]
    fn stream_reassembles_tool_calls_split_across_chunks() -> Result<()> {
        let mut state = StreamState::default();
        state.push(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{\"pa"}}
        ]}}]}));
        state.push(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"th\":\"src/main.rs\"}"}}
        ]}}]}));
        state.push(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":1,"id":"call_b","function":{"name":"list_directory","arguments":"{\"path\":\".\"}"}}
        ]}}]}));
        state.reason = "tool_calls".to_owned();
        let message = state.finish(true)?;
        let calls = message["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_a");
        assert_eq!(calls[0]["function"]["name"], "read_file");
        assert_eq!(
            calls[0]["function"]["arguments"],
            "{\"path\":\"src/main.rs\"}"
        );
        assert_eq!(calls[1]["id"], "call_b");
        assert_eq!(calls[1]["function"]["name"], "list_directory");
        Ok(())
    }

    #[test]
    fn stream_without_output_is_rejected_like_the_blocking_path() {
        let mut state = StreamState::default();
        state.push(&json!({"choices":[{"delta":{"reasoning_content":"思考中"}}]}));
        state.reason = "stop".to_owned();
        let error = state.finish(false).unwrap_err().to_string();
        assert!(error.contains("模型未返回最终回答"), "{error}");
    }

    #[test]
    fn stream_payload_and_sse_shapes() -> Result<()> {
        let messages = vec![json!({"role": "user", "content": "你好"})];
        let streamed = payload(&messages, true, true)?;
        let plain = payload(&messages, true, false)?;
        assert_eq!(
            serde_json::from_slice::<Value>(&streamed)?["stream"],
            json!(true)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&plain)?["stream"],
            json!(false)
        );
        let forced = payload(&messages, false, true)?;
        let forced: Value = serde_json::from_slice(&forced)?;
        assert_eq!(forced["tool_choice"], "none");
        assert_eq!(forced["messages"][1]["role"], "user");
        // 服务端可能带上 data: 前后空格与心跳空行，解析都要跳过。
        let body = sse(&[
            ": keep-alive",
            r#"{"choices":[{"delta":{"content":"你好"}}]}"#,
            "",
            "[DONE]",
        ]);
        assert!(body.contains("[DONE]"));
        Ok(())
    }
}
