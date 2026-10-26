mod agent;
mod auth;
mod chat;
mod model;
mod session;
mod tools;
mod tui;

use anyhow::{Result, bail};
use serde_json::json;
use std::{env, process::ExitCode};

fn main() -> ExitCode {
    let ui = tui::Ui::detect();
    match run(&ui) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            ui.error(&format!("错误：{error:#}"));
            ExitCode::FAILURE
        }
    }
}

fn run(ui: &tui::Ui) -> Result<()> {
    tui::install_interrupt_handler();
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
            "多轮对话：cargo run\n单次问答：cargo run -- \"你的问题\"\n阶段 1：cargo run --example stage1 -- \"你的问题\"\n恢复：cargo run -- --resume\n长任务：cargo run -- --max-steps 64\n对话内输入 /help 查看命令；Ctrl+C 取消当前请求。\n凭据默认读环境变量 DEEPSEEK_API_KEY；可用 RUST_AGENT_API_KEY、RUST_AGENT_ENDPOINT、RUST_AGENT_MODEL、RUST_AGENT_THINKING 覆盖。"
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
        chat::run(
            &client,
            &api_key,
            session,
            prompt == "--resume",
            max_steps,
            ui,
        )
    } else {
        let mut messages = agent::initial_messages();
        messages.push(json!({"role": "user", "content": prompt}));
        println!(
            "{}",
            agent::ask(&client, &api_key, &session, &mut messages, max_steps, ui)?
        );
        Ok(())
    }
}
