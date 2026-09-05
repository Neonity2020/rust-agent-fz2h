use super::{Tool, common::confirm};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) struct RunCommand;

fn capture(mut pipe: impl Read) -> std::io::Result<(String, bool)> {
    let mut saved = Vec::new();
    let mut chunk = [0; 4096];
    let mut truncated = false;
    loop {
        let n = pipe.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let keep = n.min((32 * 1024_usize).saturating_sub(saved.len()));
        saved.extend_from_slice(&chunk[..keep]);
        truncated |= keep < n;
    }
    Ok((String::from_utf8_lossy(&saved).into_owned(), truncated))
}

fn run(root: &Path, command: &str, timeout: Duration) -> Result<Value> {
    let mut process = Command::new("/bin/sh");
    process
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = process.spawn().context("启动命令失败")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || capture(stdout));
    let err = thread::spawn(move || capture(stderr));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(20));
    };
    // 清理同组后台子进程，关闭其持有的输出管道。
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let (stdout, out_truncated) = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout 读取线程失败"))??;
    let (stderr, err_truncated) = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr 读取线程失败"))??;
    Ok(
        json!({"exit_code":status.code(),"success":status.success() && !timed_out,"timed_out":timed_out,"stdout":stdout,"stderr":stderr,"stdout_truncated":out_truncated,"stderr_truncated":err_truncated}),
    )
}

impl Tool for RunCommand {
    fn name(&self) -> &'static str {
        "run_command"
    }
    fn description(&self) -> &'static str {
        "用户确认后在工作目录通过 /bin/sh -c 执行命令，最多 120 秒，返回退出码和输出。命令具有当前用户权限；拒绝后不要重试。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false})
    }
    fn execute(&self, root: &Path, args: &Value) -> Result<Value> {
        let command = args["command"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .context("command 必须是非空字符串")?;
        confirm(&format!(
            "工作目录：{}\n命令（当前用户权限，不是沙箱）：\n{command}",
            root.display()
        ))?;
        run(root, command, Duration::from_secs(120))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_output_and_timeout() -> Result<()> {
        let (output, truncated) = capture(std::io::Cursor::new(vec![b'a'; 100_000]))?;
        assert_eq!(output.len(), 32 * 1024);
        assert!(truncated);
        let result = run(
            Path::new("."),
            "printf hello; printf problem >&2; exit 7",
            Duration::from_secs(2),
        )?;
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["stdout"], "hello");
        assert_eq!(result["stderr"], "problem");
        assert_eq!(
            run(Path::new("."), "sleep 10", Duration::from_millis(50))?["timed_out"],
            true
        );
        Ok(())
    }
}
