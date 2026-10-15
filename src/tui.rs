// 终端渲染层：能力探测、CJK 列宽、进度行、流式输出与中断处理。
// 参照 pi-tui 的做法：状态与正文分流出错，宽度按显示列计算，非 TTY 降级为纯文本。
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    env,
    io::{IsTerminal, Write},
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
const PREVIEW_LINES: usize = 8; // 与 pi-tui 的 FALLBACK_PREVIEW_LINES 对齐
const MAX_ARG_COLUMNS: usize = 48;

static CANCEL: AtomicBool = AtomicBool::new(false);
static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

pub(crate) fn reset_cancel() {
    CANCEL.store(false, Ordering::SeqCst);
    INTERRUPTS.store(0, Ordering::SeqCst);
}

extern "C" fn on_interrupt(_: libc::c_int) {
    if INTERRUPTS.fetch_add(1, Ordering::SeqCst) >= 2 {
        // 连按三次仍未响应：恢复默认处理并重新发信号，避免卡死在长任务里。
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
            libc::raise(libc::SIGINT);
        }
    }
    CANCEL.store(true, Ordering::SeqCst);
}

/// 让 Ctrl+C 变成"取消当前请求"而不是直接杀掉进程。
/// 行编辑期 rustyline 处于 raw 模式，Ctrl+C 作为字节读入，不会产生信号。
pub(crate) fn install_interrupt_handler() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_interrupt as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

/// 东亚宽字符占两列，与 pi-tui 依赖的 get-east-asian-width 口径一致。
fn char_columns(c: char) -> usize {
    if c == '\t' {
        return 4; // 制表按四列对齐
    }
    if c.is_control() {
        return 0;
    }
    match c as u32 {
        0x1100..=0x115F            // 谚文字母
        | 0x2E80..=0x303E          // CJK 部首与假名标点
        | 0x3041..=0x33FF          // 平假名、片假名、注音
        | 0x3400..=0x4DBF          // CJK 扩展 A
        | 0x4E00..=0x9FFF          // CJK 基本区
        | 0xA000..=0xA4CF          // 彝文
        | 0xAC00..=0xD7A3          // 谚文音节
        | 0xF900..=0xFAFF          // 兼容汉字
        | 0xFE10..=0xFE19          // 竖排标点
        | 0xFE30..=0xFE6F          // 兼容形式
        | 0xFF00..=0xFF60          // 全角形式
        | 0xFFE0..=0xFFE6          // 全角符号
        | 0x1F300..=0x1F64F        // 绘文字
        | 0x1F680..=0x1F6FF
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        // 组合记号不占列。
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        _ => 1,
    }
}

pub(crate) fn display_width(text: &str) -> usize {
    text.chars().map(char_columns).sum()
}

pub(crate) fn truncate(text: &str, columns: usize) -> String {
    if display_width(text) <= columns {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let width = char_columns(c);
        if used + width > columns.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += width;
    }
    out.push('…');
    out
}

/// 按显示列换行；优先在空格处断开，中文没有空格时按字符断开，续行沿用原缩进。
pub(crate) fn wrap(text: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(8);
    let mut out = Vec::new();
    for source in text.lines() {
        if display_width(source) <= columns {
            out.push(source.to_owned());
            continue;
        }
        let (indent, body) = match source.find(|c: char| !c.is_whitespace()) {
            Some(at) if at > 0 => (source[..at].to_owned(), &source[at..]),
            _ => (String::new(), source),
        };
        // 缩进占用的列要从预算里扣掉，否则续行会顶出终端宽度。
        let columns = columns.saturating_sub(display_width(&indent)).max(1);
        let mut line = String::new();
        let mut used = 0;
        let mut split: Option<usize> = None;
        for c in body.chars() {
            let width = char_columns(c);
            if used + width > columns && !line.is_empty() {
                let (head, tail) = match split.take() {
                    Some(at) => (line[..at].trim_end().to_owned(), line[at..].trim_start().to_owned()),
                    None => (std::mem::take(&mut line), String::new()),
                };
                out.push(format!("{indent}{head}"));
                line = tail;
                used = display_width(&line);
            }
            if c == ' ' && split.is_none() {
                split = Some(line.len());
            }
            line.push(c);
            used += width;
        }
        if !line.is_empty() {
            out.push(format!("{indent}{line}"));
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

pub(crate) fn pad(text: &str, columns: usize) -> String {
    let text = truncate(text, columns);
    let filler = columns.saturating_sub(display_width(&text));
    format!("{text}{}", " ".repeat(filler))
}

/// 与 pi 的 shortenPath 一致：家目录折叠成 ~，避免长路径撑爆窄终端。
pub(crate) fn shorten_home(path: &Path) -> String {
    let text = path.display().to_string();
    match env::var_os("HOME") {
        Some(home) if !home.is_empty() => match text.strip_prefix(home.to_string_lossy().as_ref()) {
            Some(rest) => format!("~{rest}"),
            None => text,
        },
        _ => text,
    }
}

pub(crate) fn format_bytes(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 1.0 {
        format!("{:.0}ms", seconds * 1000.0)
    } else if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        format!("{}m{:02}s", (seconds / 60.0) as u64, seconds % 60.0)
    }
}

/// 折叠成 pi 里 formatToolCallWithArgs 的形式：标题后跟 key=value，值按列宽截断。
fn format_args(args: &Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    map.iter()
        .map(|(key, value)| {
            let rendered = match value {
                Value::String(text) => truncate(&text.replace(['\n', '\r'], " "), MAX_ARG_COLUMNS),
                other => truncate(&other.to_string(), MAX_ARG_COLUMNS),
            };
            format!("{key}={rendered}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() { 0 } else { text.lines().count() }
}

/// 工具结果摘要：优先给出能判断成败的信息。
fn summarize(name: &str, value: &Value) -> String {
    let bool_field = |key: &str| value[key].as_bool().unwrap_or(false);
    match name {
        "read_file" => {
            let text = value["text"].as_str().unwrap_or_default();
            format!("{} 行 · {}", count_lines(text), format_bytes(text.len()))
        }
        "write_file" | "replace_text" => {
            if !bool_field("changed") {
                return "内容未变化，未写入".to_owned();
            }
            let path = value["path"].as_str().unwrap_or("?");
            if bool_field("created") {
                format!("新建 {path}")
            } else {
                format!("已更新 {path}")
            }
        }
        "search_text" => {
            let found = value["matches"].as_array().map_or(0, Vec::len);
            let mut text = format!("{found} 处匹配");
            if bool_field("truncated") {
                text.push_str("（已达预算上限）");
            }
            let skipped = value["skipped"].as_u64().unwrap_or(0);
            if skipped > 0 {
                text.push_str(&format!("，跳过 {skipped} 个文件"));
            }
            text
        }
        "list_directory" => {
            let count = value["entries"].as_array().map_or(0, Vec::len);
            let mut text = format!("{count} 个条目");
            if bool_field("truncated") {
                text.push_str("（已截断）");
            }
            text
        }
        "run_command" => {
            let stdout = count_lines(value["stdout"].as_str().unwrap_or_default().trim_end());
            let stderr = count_lines(value["stderr"].as_str().unwrap_or_default().trim_end());
            let status = if bool_field("timed_out") {
                "超时".to_owned()
            } else if bool_field("success") {
                "成功".to_owned()
            } else {
                format!("退出码 {}", value["exit_code"].as_i64().unwrap_or(-1))
            };
            format!("{status} · stdout {stdout} 行 · stderr {stderr} 行")
        }
        _ => format_bytes(value.to_string().len()),
    }
}

/// 输出尾部预览：让用户直接看到编译错误一类的东西，而不是只看到一个退出码。
fn tail_preview(name: &str, value: &Value) -> Vec<String> {
    if name != "run_command" {
        return Vec::new();
    }
    let stdout = value["stdout"].as_str().unwrap_or_default().trim_end();
    let stderr = value["stderr"].as_str().unwrap_or_default().trim_end();
    let text = [stdout, stderr]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = text.lines().map(|line| line.trim_end().to_owned()).collect();
    if lines.len() > PREVIEW_LINES {
        let hidden = lines.len() - PREVIEW_LINES;
        lines.drain(..hidden);
        lines.insert(0, format!("…（省略 {hidden} 行）"));
    }
    lines
}

pub(crate) struct Ui {
    interactive: bool,
    color: bool,
    // 测试把状态重定向到内存，其余场景直接写终端。
    buffer: Option<Rc<RefCell<String>>>,
    streaming: Cell<bool>,
    busy: Cell<Option<(usize, Instant)>>,
    stream_started: Cell<Instant>,
}

impl Ui {
    pub fn detect() -> Self {
        let interactive = std::io::stderr().is_terminal();
        let color = std::io::stdout().is_terminal()
            && env::var_os("NO_COLOR").is_none()
            && !env::var("TERM").is_ok_and(|term| term.is_empty() || term == "dumb");
        Self {
            interactive,
            color,
            buffer: None,
            streaming: Cell::new(false),
            busy: Cell::new(None),
            stream_started: Cell::new(Instant::now()),
        }
    }

    #[cfg(test)]
    fn silent() -> Self {
        Self {
            interactive: false,
            color: false,
            buffer: Some(Rc::new(RefCell::new(String::new()))),
            streaming: Cell::new(false),
            busy: Cell::new(None),
            stream_started: Cell::new(Instant::now()),
        }
    }

    /// 测试与非交互场景使用：不重绘、不着色、不写终端。
    #[cfg(test)]
    pub fn quiet() -> Self {
        Self::silent()
    }

    /// 渲染结果重定向到内存，便于断言终端上真正会出现什么。
    #[cfg(test)]
    pub fn capture() -> (Self, Rc<RefCell<String>>) {
        let buffer = Rc::new(RefCell::new(String::new()));
        let ui = Self {
            interactive: true,
            color: true,
            buffer: Some(Rc::clone(&buffer)),
            streaming: Cell::new(false),
            busy: Cell::new(None),
            stream_started: Cell::new(Instant::now()),
        };
        (ui, buffer)
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    fn columns(&self) -> usize {
        terminal_width().saturating_sub(4).max(24)
    }

    /// 状态行走 stderr，正文走 stdout，重定向到文件时不会互相污染。
    fn emit(&self, text: &str, newline: bool) {
        if let Some(buffer) = &self.buffer {
            buffer.borrow_mut().push_str(text);
            if newline {
                buffer.borrow_mut().push('\n');
            }
            return;
        }
        if newline {
            let mut err = std::io::stderr();
            let _ = writeln!(err, "{text}");
            let _ = err.flush();
        } else {
            let mut out = std::io::stdout();
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
        }
    }

    fn status(&self, text: &str) {
        self.emit(text, true);
    }

    pub fn note(&self, text: &str) {
        self.status(&self.paint("2;37", text));
    }

    pub fn warn(&self, text: &str) {
        self.status(&self.paint("33", text));
    }

    pub fn error(&self, text: &str) {
        self.status(&self.paint("1;31", text));
    }

    pub fn success(&self, text: &str) {
        self.status(&self.paint("32", text));
    }

    /// 清掉进度行，避免和后续输出叠在一起。
    fn clear_busy(&self) {
        if self.busy.take().is_some() && self.interactive && self.buffer.is_none() {
            let mut err = std::io::stderr();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }

    pub fn busy_start(&self, label: &str) {
        if !self.interactive {
            return;
        }
        self.busy.set(Some((0, Instant::now())));
        self.draw_busy(label);
    }

    fn draw_busy(&self, label: &str) {
        let Some((frame, _)) = self.busy.get() else {
            return;
        };
        #[cfg(test)]
        if self.buffer.is_some() {
            return;
        }
        let line = format!(
            "{} {}",
            self.paint("36", SPINNER[frame % SPINNER.len()]),
            self.paint("2", &truncate(label, self.columns().saturating_sub(2)))
        );
        let mut err = std::io::stderr();
        let _ = write!(err, "\r\x1b[2K{line}");
        let _ = err.flush();
    }

    /// 等待网络或子进程时调用；间隔太短就不重绘，避免刷屏。
    pub fn tick(&self, label: &str) {
        let Some((frame, at)) = self.busy.get() else {
            return;
        };
        if at.elapsed() < Duration::from_millis(90) {
            return;
        }
        self.busy.set(Some((frame + 1, Instant::now())));
        self.draw_busy(label);
    }

    pub fn busy_stop(&self) {
        self.clear_busy();
    }

    pub fn step(&self, step: usize, max: usize, summarizing: bool) {
        let label = format!(
            "步骤 {step}/{max}{}",
            if summarizing { " · 收尾总结，不再调用工具" } else { "" }
        );
        self.status(&self.paint("2;37", &label));
    }

    pub fn tool_call(&self, name: &str, args: &Value) {
        self.clear_busy();
        let mark = self.paint("1;36", "◆");
        let mut body = String::new();
        let args = format_args(args);
        if !args.is_empty() {
            body = format!(
                " {}",
                self.paint("2", &truncate(&args, self.columns().saturating_sub(name.len() + 4)))
            );
        }
        self.status(&format!("  {mark} {name}{body}"));
    }

    pub fn tool_result(&self, name: &str, result: &std::result::Result<Value, anyhow::Error>, elapsed: Duration) {
        self.clear_busy();
        match result {
            Ok(value) => {
                self.status(&format!(
                    "    {} {} · {}",
                    self.paint("32", "✓"),
                    self.paint("2", &truncate(&summarize(name, value), self.columns().saturating_sub(12))),
                    self.paint("2", &format_elapsed(elapsed))
                ));
                for line in tail_preview(name, value) {
                    let body = wrap(&line, self.columns());
                    for (index, part) in body.iter().enumerate() {
                        self.status(&format!(
                            "      {}{}",
                            if index == 0 { "" } else { "  " },
                            self.paint("2;37", part)
                        ));
                    }
                }
            }
            Err(error) => {
                let text = wrap(&format!("{error:#}"), self.columns().saturating_sub(6));
                for (index, line) in text.iter().enumerate() {
                    self.status(&format!(
                        "    {}{} {}",
                        self.paint("1;31", "✗"),
                        if index == 0 { "" } else { "  " },
                        self.paint("31", line)
                    ));
                }
            }
        }
    }

    pub fn delta(&self, text: &str) {
        if !self.streaming.replace(true) {
            self.stream_started.set(Instant::now());
        }
        self.emit(text, false);
    }

    /// 流式结束后补换行，并回报本次生成耗时。
    pub fn stream_end(&self, aborted: bool) {
        if !self.streaming.replace(false) {
            return;
        }
        let elapsed = self.stream_started.get().elapsed();
        self.emit("\n", false);
        let mark = if aborted {
            self.paint("33", "已中断")
        } else {
            self.paint("2", &format_elapsed(elapsed))
        };
        self.status(&format!("    {mark}"));
    }
}

pub(crate) fn terminal_width() -> usize {
    #[cfg(unix)]
    unsafe {
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 {
                return size.ws_col as usize;
            }
        }
    }
    80
}

/// 命令清单：REPL 的 /help 与启动横幅共用一份，避免两处漂移。
pub(crate) fn command_help() -> &'static [(&'static str, &'static str)] {
    &[
        ("/help", "显示本清单"),
        ("/status", "查看模型、会话与上下文占用"),
        ("/save", "把当前对话存为独立存档"),
        ("/load", "读取 /save 存档"),
        ("/recover", "读取自动检查点（含中断时的半成品历史）"),
        ("/continue", "让助手接着上一轮未完成的工作"),
        ("/clear", "清空上下文并开新会话"),
        ("/exit", "退出，Ctrl+D 同样有效"),
    ]
}

pub(crate) fn interrupt_hint() -> &'static str {
    "Ctrl+C 取消当前请求，在输入框里则清空当前行；连按三次强制退出。"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_count_cjk_as_two_columns() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("中文"), 4);
        assert_eq!(display_width("a中b"), 4);
        assert_eq!(display_width("\t中"), 6);
    }

    #[test]
    fn truncate_respects_columns_and_marks_ellipsis() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        assert_eq!(truncate("中文测试", 4), "中…");
        assert!(display_width(&truncate("中文测试内容", 6)) <= 6);
        assert_eq!(truncate("中文", 3), "中…");
    }

    #[test]
    fn wrap_keeps_every_line_within_columns() {
        let text = "第一行很短\n第二行非常长需要换行处理，否则会超出终端宽度限制";
        let lines = wrap(text, 12);
        assert!(lines.len() > 2, "应当换行：{lines:?}");
        for line in &lines {
            assert!(display_width(line) <= 12, "越界：{line}");
        }
        assert!(lines.iter().any(|line| line.starts_with("第二行")));
    }

    #[test]
    fn wrap_breaks_on_spaces_and_keeps_indent() {
        let lines = wrap("cargo build --release now", 12);
        assert!(lines.len() >= 2);
        for line in lines {
            assert!(display_width(&line) <= 12);
        }
        let indented = wrap("    很长的一段带缩进的中文文本需要换行处理", 12);
        assert!(indented.len() > 1);
        for line in indented {
            assert!(display_width(&line) <= 12);
            assert!(line.starts_with("    "), "续行应保留缩进：{line}");
        }
    }

    #[test]
    fn pad_aligns_by_columns_not_bytes() {
        assert_eq!(display_width(&pad("中文", 10)), 10);
        assert_eq!(display_width(&pad("工具/命令", 6)), 6);
        assert_eq!(pad("abc", 6), "abc   ");
    }

    #[test]
    fn call_format_folds_arguments_to_one_line() {
        let args = serde_json::json!({"path": "src/main.rs", "line": 12});
        assert_eq!(format_args(&args), "line=12 path=src/main.rs");
        let long = serde_json::json!({"command": "cargo build\n--release"});
        assert!(!format_args(&long).contains('\n'));
        assert_eq!(format_args(&serde_json::json!({})), "");
    }

    #[test]
    fn summaries_match_tool_result_shapes() {
        assert_eq!(
            summarize("read_file", &serde_json::json!({"text": "a\nb\nc"})),
            "3 行 · 5 B"
        );
        assert_eq!(
            summarize("write_file", &serde_json::json!({"changed": true, "created": true, "path": "a.rs"})),
            "新建 a.rs"
        );
        assert_eq!(
            summarize("replace_text", &serde_json::json!({"changed": false})),
            "内容未变化，未写入"
        );
        assert_eq!(
            summarize("search_text", &serde_json::json!({"matches": [{"line": 1}], "truncated": true, "skipped": 3})),
            "1 处匹配（已达预算上限），跳过 3 个文件"
        );
        let run = summarize(
            "run_command",
            &serde_json::json!({"exit_code": 101, "success": false, "timed_out": false, "stdout": "", "stderr": "err\n"}),
        );
        assert!(run.starts_with("退出码 101"), "{run}");
    }

    #[test]
    fn run_command_preview_shows_output_tail() {
        let value = serde_json::json!({"stdout": "", "stderr": "error[E0432]: 未解析的导入\n"});
        assert_eq!(tail_preview("run_command", &value), vec!["error[E0432]: 未解析的导入"]);
        assert!(tail_preview("read_file", &value).is_empty());

        let stdout = (1..=20).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        let value = serde_json::json!({"stdout": stdout, "stderr": "", "success": true, "exit_code": 0});
        let preview = tail_preview("run_command", &value);
        assert_eq!(preview.len(), PREVIEW_LINES + 1);
        assert!(preview[0].contains("省略 12 行"), "{preview:?}");
        assert!(preview.last().unwrap().contains("line 20"));
    }

    #[test]
    fn human_units_and_elapsed_are_readable() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_elapsed(Duration::from_millis(12)), "12ms");
        assert_eq!(format_elapsed(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_elapsed(Duration::from_secs(75)), "1m15s");
    }

    #[test]
    fn quiet_ui_never_emits_color_escapes() {
        let ui = Ui::quiet();
        assert!(!ui.is_interactive());
        assert_eq!(ui.paint("31", "错误"), "错误");
        assert_eq!(ui.paint("31", "错误"), ui.paint("32", "错误"));
    }

    #[test]
    fn home_is_shortened_like_pi() {
        let Some(home) = env::var_os("HOME").map(std::path::PathBuf::from).filter(|p| !p.as_os_str().is_empty()) else {
            return; // 环境里没有 HOME 时无可折叠的前缀
        };
        assert_eq!(
            shorten_home(&home.join("workspace/rust-agent")),
            "~/workspace/rust-agent"
        );
        assert_eq!(shorten_home(Path::new("/etc/hosts")), "/etc/hosts");
    }
    #[test]
    fn rendered_transcript_stays_within_the_terminal_width() {
        let (ui, buffer) = Ui::capture();
        let columns = terminal_width().saturating_sub(4).max(24);
        ui.step(3, 32, true);
        ui.tool_call("run_command", &serde_json::json!({"command": "cargo clippy --all-targets 2>&1 | tail -20"}));
        let stderr = (0..14)
            .map(|n| format!("warning: 这是一条很长很长的中文编译错误说明第 {n} 行，需要换行"))
            .collect::<Vec<_>>()
            .join("\n");
        ui.tool_result(
            "run_command",
            &Ok(serde_json::json!({"exit_code": 101, "success": false, "timed_out": false, "stdout": "", "stderr": stderr})),
            Duration::from_secs(3),
        );
        ui.tool_result("write_file", &Err(anyhow::anyhow!("路径超出工作目录：../../etc/passwd")), Duration::from_millis(2));
        let out = buffer.borrow().clone();
        assert!(out.contains("省略 6 行"), "{out}");
        assert!(out.contains("退出码 101") && out.contains("3.0s"), "{out}");
        assert!(out.contains("✗") && out.contains("路径超出工作目录"), "{out}");
        for line in out.lines() {
            let width = display_width(&strip_ansi(line));
            assert!(width <= columns, "超出终端宽度 {columns}：{width} {line}");
        }
    }

    /// 去掉 SGR 转义序列后再量宽度，否则 ANSI 码会被算成可见列。
    fn strip_ansi(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        }
        out
    }
}
