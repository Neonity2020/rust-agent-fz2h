use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{
    env,
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ENDPOINT: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const MODEL: &str = "omen-alpha";

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
    let args: Vec<String> = env::args().skip(1).collect();
    let prompt = args.join(" ");
    if prompt == "--help" || prompt == "-h" {
        println!(
            "多轮对话：cargo run --example stage2\n单次问答：cargo run --example stage2 -- \"你的问题\"\n阶段 1：cargo run --example stage1 -- \"你的问题\"\n对话命令：/clear 清空会话，/exit 退出\n自动读取本机 OpenCode auth.json 中的 Go 凭据。"
        );
        return Ok(());
    }
    if !args.is_empty() && prompt.trim().is_empty() {
        bail!("请输入问题，例如：cargo run -- \"解释 Rust 的所有权\"");
    }
    let api_key = load_api_key()?;

    let session = new_session()?;
    let client = Client::builder()
        .user_agent(concat!("rust-agent/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()
        .context("创建 HTTP 客户端失败")?;

    if args.is_empty() {
        chat(&client, &api_key, session)
    } else {
        let messages = vec![json!({"role": "user", "content": prompt})];
        println!("{}", ask(&client, &api_key, &session, &messages)?);
        Ok(())
    }
}

fn new_session() -> Result<String> {
    Ok(format!(
        "rust-agent-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ))
}

fn initial_messages() -> Vec<Value> {
    vec![json!({
        "role": "system",
        "content": "你是一个 Rust 编程助手。请用中文清晰简洁地回答。你目前只能对话，不能读取文件、修改代码或执行命令。"
    })]
}

fn chat(client: &Client, api_key: &str, mut session: String) -> Result<()> {
    use rustyline::{DefaultEditor, error::ReadlineError};

    let mut messages = initial_messages();
    let mut editor = DefaultEditor::new().context("初始化行编辑器失败")?;
    eprintln!("Omen Alpha 多轮对话。/clear 清空会话，/exit 退出；Ctrl-D 结束输入。");
    loop {
        let input = match editor.readline("你> ") {
            Ok(input) => input,
            Err(ReadlineError::Interrupted) => {
                eprintln!("已取消当前输入。");
                continue;
            }
            Err(ReadlineError::Eof) => return Ok(()),
            Err(error) => return Err(error).context("读取输入失败"),
        };
        let prompt = input.trim();
        match prompt {
            "" => continue,
            "/exit" => return Ok(()),
            "/clear" => {
                messages = initial_messages();
                session = new_session()?;
                eprintln!("会话已清空。");
                continue;
            }
            _ => {}
        }
        messages.push(json!({"role": "user", "content": prompt}));
        match ask(client, api_key, &session, &messages) {
            Ok(answer) => {
                println!("助手> {answer}");
                messages.push(json!({"role": "assistant", "content": answer}));
            }
            Err(error) => {
                // 只保留完整问答，失败的输入不进入下一轮上下文。
                messages.pop();
                eprintln!("错误：{error:#}\n本轮未加入历史，可以重新输入。");
            }
        }
    }
}

fn ask(client: &Client, api_key: &str, session: &str, messages: &[Value]) -> Result<String> {
    eprintln!("正在请求 {MODEL}…");
    let response = client
        .post(ENDPOINT)
        .bearer_auth(api_key)
        .header("x-opencode-session", session)
        .json(&json!({
            "model": MODEL,
            "messages": messages,
            "stream": false
        }))
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
    let body: Value = response.json().context("接口返回的内容不是有效 JSON")?;
    let answer = body["choices"][0]["message"]["content"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .context("接口未返回非空的 choices[0].message.content 文本")?;
    Ok(answer.to_owned())
}

fn load_api_key() -> Result<String> {
    let data_dir = match env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        Some(path) => std::path::PathBuf::from(path),
        None => std::path::PathBuf::from(env::var_os("HOME").context("无法确定用户主目录")?)
            .join(".local/share"),
    };
    let path = data_dir.join("opencode/auth.json");
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("无法读取 {}，请先在 OpenCode 中登录 Go", path.display()))?;
    let auth: Value = serde_json::from_str(&contents).context("OpenCode auth.json 格式无效")?;
    let go = &auth["opencode-go"];
    if go["type"].as_str() != Some("api") {
        bail!("auth.json 缺少 Go API 凭据，请先在 OpenCode 中连接 OpenCode Go");
    }
    let key = go["key"]
        .as_str()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .context("auth.json 中的 Go API Key 缺失或为空")?;
    Ok(key.to_owned())
}
