use crate::{
    agent::{ask, initial_messages},
    model::MODEL,
    session,
};
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use std::{
    env,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn new_session() -> Result<String> {
    Ok(format!(
        "rust-agent-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ))
}

pub(crate) fn run(
    client: &Client,
    api_key: &str,
    mut session: String,
    resume: bool,
    max_steps: usize,
) -> Result<()> {
    use rustyline::{DefaultEditor, error::ReadlineError};

    let mut messages = initial_messages();
    let root = env::current_dir()?.canonicalize()?;
    let session_path = root.join(".rust-agent/session.json");
    if resume {
        (session, messages) = session::load(
            &if root.join(".rust-agent/checkpoint.json").exists() {
                root.join(".rust-agent/checkpoint.json")
            } else {
                session_path.clone()
            },
            &root,
        )?;
        messages[0] = initial_messages().remove(0);
        eprintln!("已恢复 {} 条消息。", messages.len());
    }
    let mut editor = DefaultEditor::new().context("初始化行编辑器失败")?;
    eprintln!(
        "工作目录：{}\n/save 保存，/load 恢复，/status 状态，/clear 清空，/continue 继续，/recover 恢复检查点，/exit 退出。任务自动保存检查点，/save 保存独立存档。",
        root.display()
    );
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
            "/save" => {
                match session::save(&session_path, &root, &session, &messages) {
                    Ok(()) => eprintln!("会话已保存至 {}", session_path.display()),
                    Err(error) => eprintln!("保存失败：{error:#}"),
                }
                continue;
            }
            "/load" | "/recover" => {
                let path = if prompt == "/recover" {
                    root.join(".rust-agent/checkpoint.json")
                } else {
                    session_path.clone()
                };
                match session::load(&path, &root) {
                    Ok((id, history)) => {
                        session = id;
                        messages = history;
                        messages[0] = initial_messages().remove(0);
                        eprintln!("已恢复 {} 条消息。", messages.len());
                    }
                    Err(error) => eprintln!("恢复失败（当前会话保留）：{error:#}"),
                }
                continue;
            }
            "/status" => {
                eprintln!(
                    "模型：{MODEL}，消息：{}，历史 JSON：{} 字节，每轮最多 {max_steps} 步。",
                    messages.len(),
                    serde_json::to_vec(&messages)?.len()
                );
                continue;
            }
            "/clear" => {
                messages = initial_messages();
                session = new_session()?;
                eprintln!("会话已清空。");
                continue;
            }
            _ => {}
        }
        let prompt = if prompt == "/continue" {
            "请继续未完成的任务。先核实当前文件状态和之前操作结果，不要重复已完成的修改；完成后验证并汇报。"
        } else {
            prompt
        };
        messages.push(json!({"role": "user", "content": prompt}));
        match ask(client, api_key, &session, &mut messages, max_steps) {
            Ok(answer) => {
                println!("助手> {answer}");
                messages.push(json!({"role": "assistant", "content": answer}));
            }
            Err(error) => {
                // 保留已执行操作的记录，恢复时检查实际状态。

                eprintln!(
                    "错误：{error:#}\n已保留本轮历史与检查点。可 /continue 继续；重启用 --resume。已执行操作不会自动撤销。"
                );
            }
        }
    }
}
