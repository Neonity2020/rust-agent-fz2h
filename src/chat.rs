use crate::{
    agent::{ask, initial_messages},
    model, session,
    tui::{self, Ui, command_help, format_bytes, interrupt_hint, pad, shorten_home},
};
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use rustyline::DefaultEditor;
use serde_json::{Value, json};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_HISTORY: usize = 500;

pub(crate) fn new_session() -> Result<String> {
    Ok(format!(
        "rust-agent-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ))
}

// 输入历史自己落盘：rustyline 只留内存里最近的一百条负责上下键，
// 文件负责跨会话召回，写入时用 0600，避免提示词被同机其他用户读到。
fn history_path(state: &Path) -> PathBuf {
    state.join("history")
}

fn rewrite_history(path: &Path, lines: &[String]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).context("写入输入历史失败")?;
    for line in lines {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

fn load_history(editor: &mut DefaultEditor, path: &Path, ui: &Ui) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<String> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    let keep = lines.len().saturating_sub(MAX_HISTORY);
    for line in &lines {
        if let Err(error) = editor.add_history_entry(line) {
            ui.warn(&format!("输入历史未载入：{error}"));
            return;
        }
    }
    if keep > 0 {
        let _ = rewrite_history(path, &lines[keep..]);
    }
}

fn append_history(path: &Path, line: &str) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(path) {
        let _ = writeln!(file, "{}", line.replace(['\n', '\r'], " "));
    }
}

fn banner(ui: &Ui, root: &Path, max_steps: usize) {
    let version = env!("CARGO_PKG_VERSION");
    let directory = shorten_home(root);
    let commands = format!(
        "输入 /help 查看命令，每轮最多 {max_steps} 步。{}",
        interrupt_hint()
    );
    ui.note(&format!(
        "rust-agent {version} · {} · 工作目录 {directory}",
        model::model()
    ));
    ui.note(&commands);
}

fn status_report(
    messages: &[Value],
    session: &str,
    root: &Path,
    max_steps: usize,
    session_path: &Path,
) -> Result<Vec<String>> {
    let bytes = serde_json::to_vec(messages)?.len();
    let calls: usize = messages
        .iter()
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .count();
    let row = |label: &str, value: String| format!("  {}  {}", pad(label, 8), value);
    let provider = model::provider_name();
    Ok(vec![
        row("模型", format!("{}（{provider}）", model::model())),
        row("工作目录", shorten_home(root)),
        row("会话", session.to_owned()),
        row("存档", shorten_home(session_path)),
        row(
            "历史",
            format!(
                "{} 条 · {calls} 次工具调用 · 上下文 {}",
                messages.len(),
                format_bytes(bytes)
            ),
        ),
        row("步数", format!("每轮最多 {max_steps} 步")),
    ])
}

pub(crate) fn run(
    client: &Client,
    api_key: &str,
    mut session: String,
    resume: bool,
    max_steps: usize,
    ui: &Ui,
) -> Result<()> {
    let mut messages = initial_messages();
    let root = env::current_dir()?.canonicalize()?;
    let state = root.join(".rust-agent");
    let session_path = state.join("session.json");
    let checkpoint_path = state.join("checkpoint.json");
    let history = history_path(&state);
    if resume {
        let source = if checkpoint_path.exists() {
            &checkpoint_path
        } else {
            &session_path
        };
        match session::load(source, &root) {
            Ok((id, mut history_messages)) => {
                history_messages[0] = initial_messages().remove(0);
                session = id;
                messages = history_messages;
                ui.success(&format!(
                    "已从 {} 恢复 {} 条消息。",
                    source.file_name().unwrap_or_default().to_string_lossy(),
                    messages.len()
                ));
            }
            Err(error) => ui.warn(&format!("恢复失败（将开新会话）：{error:#}")),
        }
    }

    let mut editor = DefaultEditor::new().context("初始化行编辑器失败")?;
    load_history(&mut editor, &history, ui);
    banner(ui, &root, max_steps);

    let mut interrupts = 0;
    loop {
        let input = match editor.readline("你> ") {
            Ok(input) => input,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                tui::reset_cancel();
                interrupts += 1;
                if interrupts >= 2 {
                    return Ok(());
                }
                ui.warn("已取消当前输入，再按一次 Ctrl+C 退出。");
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => return Ok(()),
            Err(error) => return Err(error).context("读取输入失败"),
        };
        let prompt = input.trim();
        if prompt.is_empty() {
            interrupts = 0;
            continue;
        }
        interrupts = 0;
        if let Err(error) = editor.add_history_entry(prompt) {
            ui.warn(&format!("输入历史未更新：{error}"));
        }
        append_history(&history, prompt);

        match prompt {
            "/exit" | "/quit" => return Ok(()),
            "/help" | "/?" => {
                ui.note("可用命令：");
                for (name, description) in command_help() {
                    ui.note(&format!("  {}  {description}", pad(name, 10)));
                }
                ui.note(&format!("  {}", interrupt_hint()));
                continue;
            }
            "/save" => match session::save(&session_path, &root, &session, &messages) {
                Ok(()) => ui.success(&format!("会话已保存至 {}", shorten_home(&session_path))),
                Err(error) => ui.error(&format!("保存失败：{error:#}")),
            },
            "/load" | "/recover" => {
                let path = if prompt == "/recover" {
                    &checkpoint_path
                } else {
                    &session_path
                };
                match session::load(path, &root) {
                    Ok((id, mut restored)) => {
                        restored[0] = initial_messages().remove(0);
                        session = id;
                        messages = restored;
                        ui.success(&format!("已恢复 {} 条消息。", messages.len()));
                    }
                    Err(error) => ui.error(&format!("恢复失败（当前会话保留）：{error:#}")),
                }
                continue;
            }
            "/status" => {
                match status_report(&messages, &session, &root, max_steps, &session_path) {
                    Ok(lines) => {
                        ui.note("");
                        for line in lines {
                            ui.note(&line);
                        }
                        ui.note("");
                    }
                    Err(error) => ui.error(&format!("状态读取失败：{error:#}")),
                }
            }
            "/clear" => {
                messages = initial_messages();
                session = new_session()?;
                ui.success("会话已清空。");
            }
            _ => {}
        }
        // 未知斜杠命令按命令处理，不当作问题发给模型。
        if prompt.starts_with('/') && prompt != "/continue" {
            continue;
        }

        let prompt = if prompt == "/continue" {
            "请继续未完成的任务。先核实当前文件状态和之前操作结果，不要重复已完成的修改；完成后验证并汇报。"
        } else {
            prompt
        };
        messages.push(json!({"role": "user", "content": prompt}));
        match ask(client, api_key, &session, &mut messages, max_steps, ui) {
            Ok(answer) => {
                if !ui.is_interactive() {
                    println!("{answer}");
                }
                messages.push(json!({"role": "assistant", "content": answer}));
            }
            Err(error) => {
                // 保留已执行操作的记录，恢复时检查实际状态。
                ui.error(&format!("错误：{error:#}"));
                ui.note("本轮历史与检查点已保留。可 /continue 继续，重启用 --resume；已执行操作不会自动撤销。");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyline::history::{History, SearchDirection};

    #[test]
    fn status_report_is_human_readable() -> Result<()> {
        let messages = vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "user", "content": "改一下"}),
            json!({"role": "assistant", "tool_calls": [{"id": "1"}, {"id": "2"}]}),
            json!({"role": "tool", "tool_call_id": "1", "content": "{}"}),
            json!({"role": "tool", "tool_call_id": "2", "content": "{}"}),
        ];
        let lines = status_report(
            &messages,
            "s-1",
            Path::new("/tmp/x"),
            32,
            Path::new("/tmp/x/.rust-agent/session.json"),
        )?;
        let text = lines.join("\n");
        assert!(text.contains("2 次工具调用"), "{text}");
        assert!(text.contains("deepseek-flash"));
        assert!(text.contains("每轮最多 32 步"));
        assert!(text.contains("5 条"), "{text}");
        Ok(())
    }

    #[test]
    fn history_round_trips_through_the_file() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("rust-agent-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = history_path(&dir);
        let mut editor = DefaultEditor::new()?;
        load_history(&mut editor, &path, &Ui::quiet());
        assert_eq!(editor.history().len(), 0);
        for line in ["第一句", "second line"] {
            let _ = editor.add_history_entry(line);
            append_history(&path, line);
        }
        let mut restored = DefaultEditor::new()?;
        load_history(&mut restored, &path, &Ui::quiet());
        assert_eq!(restored.history().len(), 2);
        // rustyline 的历史按写入顺序存放，最后一条才是最近输入。
        let newest = restored.history().len() - 1;
        assert_eq!(
            restored
                .history()
                .get(newest, SearchDirection::Forward)
                .expect("查询历史")
                .expect("最近一条")
                .entry,
            "second line"
        );
        let _ = fs::remove_dir_all(dir);
        Ok(())
    }

    #[test]
    fn oversized_history_is_trimmed_to_the_tail() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("rust-agent-trim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        let path = history_path(&dir);
        let lines: Vec<String> = (0..MAX_HISTORY + 20).map(|n| format!("line {n}")).collect();
        rewrite_history(&path, &lines)?;
        let mut editor = DefaultEditor::new()?;
        load_history(&mut editor, &path, &Ui::quiet());
        // 内存里只留 rustyline 记得住的部分，文件保留全部。
        assert_eq!(fs::read_to_string(&path)?.lines().count(), MAX_HISTORY);
        let _ = fs::remove_dir_all(dir);
        Ok(())
    }
}
