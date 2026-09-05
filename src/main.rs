mod agent;
mod auth;
mod chat;
mod model;
mod session;
mod tools;

use anyhow::{Result, bail};
use serde_json::json;
use std::{env, process::ExitCode};

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
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut max_steps = agent::MAX_STEPS;
    if args.first().is_some_and(|s| s == "--max-steps") {
        if args.len() < 2 {
            bail!("--max-steps 需要整数参数");
        }
        max_steps = args[1].parse()?;
        if !(2..=256).contains(&max_steps) {
            bail!("步数须为 2–256");
        }
        args.drain(..2);
    }
    let prompt = args.join(" ");
    if prompt == "--help" || prompt == "-h" {
        println!(
            "多轮对话：cargo run\n单次问答：cargo run -- \"你的问题\"\n阶段 1：cargo run --example stage1 -- \"你的问题\"\n恢复：cargo run -- --resume\n长任务：cargo run -- --max-steps 64\n对话命令：/save 保存，/load 恢复，/status 状态，/continue 继续，/recover 恢复检查点，/clear 清空，/exit 退出\n自动读取本机 OpenCode auth.json 中的 Go 凭据。"
        );
        return Ok(());
    }
    if !args.is_empty() && prompt.trim().is_empty() {
        bail!("请输入问题，例如：cargo run -- \"解释 Rust 的所有权\"");
    }
    let api_key = auth::load_api_key()?;

    let session = chat::new_session()?;
    let client = model::create_client()?;

    if args.is_empty() || prompt == "--resume" {
        chat::run(&client, &api_key, session, prompt == "--resume", max_steps)
    } else {
        let mut messages = agent::initial_messages();
        messages.push(json!({"role": "user", "content": prompt}));
        println!(
            "{}",
            agent::ask(&client, &api_key, &session, &mut messages, max_steps)?
        );
        Ok(())
    }
}
