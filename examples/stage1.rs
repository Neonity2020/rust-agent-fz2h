use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{
    env,
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ENDPOINT: &str = "https://api.deepseek.com/chat/completions";
const MODEL: &str = "deepseek-flash";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    if prompt == "--help" || prompt == "-h" {
        println!(
            "用法：cargo run --example stage1 -- \"你的问题\"\n凭据从环境变量 DEEPSEEK_API_KEY 读取。"
        );
        return Ok(());
    }
    if prompt.trim().is_empty() {
        bail!("请输入问题，例如：cargo run --example stage1 -- \"解释 Rust 的所有权\"");
    }
    let api_key = load_api_key()?;

    // 第一阶段每次启动只有一次请求，因此每次生成一个会话标识。
    let session = format!(
        "rust-agent-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let client = Client::builder()
        .user_agent(concat!("rust-agent/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()
        .context("创建 HTTP 客户端失败")?;

    eprintln!("正在请求 {MODEL}…");
    let response = client
        .post(ENDPOINT)
        .bearer_auth(api_key)
        .header("x-session-id", session)
        .json(&json!({
            "model": MODEL,
            "messages": [{"role": "user", "content": prompt}],
            "stream": false
        }))
        .send()
        .context("请求失败，请检查网络；请求最长等待 180 秒")?;

    let status = response.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => "请检查 API Key、账户余额和模型访问权限",
            429 => "请求受限，请检查账户额度或稍后重试",
            500..=599 => "模型服务暂时异常，请稍后重试",
            _ => "请检查接口参数和模型可用性",
        };
        bail!("DeepSeek 返回 HTTP {status}：{hint}");
    }
    let body: Value = response.json().context("接口返回的内容不是有效 JSON")?;
    let answer = body["choices"][0]["message"]["content"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .context("接口未返回非空的 choices[0].message.content 文本")?;
    println!("{answer}");
    Ok(())
}

fn load_api_key() -> Result<String> {
    for name in ["RUST_AGENT_API_KEY", "DEEPSEEK_API_KEY"] {
        if let Some(value) = env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            return Ok(value);
        }
    }
    bail!(
        "未找到 API Key：请设置环境变量 DEEPSEEK_API_KEY（或 RUST_AGENT_API_KEY），\
         密钥可在 https://platform.deepseek.com/api_keys 申请"
    )
}
