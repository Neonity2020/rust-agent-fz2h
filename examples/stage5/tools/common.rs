use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub(super) const MAX_BYTES: u64 = 64 * 1024;

pub(super) fn confirm(preview: &str) -> Result<()> {
    use std::io::{self, IsTerminal};
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!("需要交互终端确认，本次操作未执行");
    }
    // 转义控制字符，避免预览文本改变终端状态或伪造确认提示。
    let safe: String = preview
        .chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect();
    eprintln!("{safe}");
    let mut editor = rustyline::DefaultEditor::new()?;
    match editor.readline("允许本次操作？输入 yes 确认，其他输入取消> ") {
        Ok(answer) if answer.trim() == "yes" => Ok(()),
        _ => bail!("用户取消，本次操作未执行；不要重复请求同一操作"),
    }
}
pub(super) fn resolve(root: &Path, path: &str) -> Result<PathBuf> {
    let resolved = root
        .join(path)
        .canonicalize()
        .context("路径不存在或无法访问")?;
    if !resolved.starts_with(root) {
        bail!("路径超出工作目录");
    }
    Ok(resolved)
}

pub(super) fn read_text(path: &Path) -> Result<String> {
    if !path.is_file() {
        bail!("只能读取普通文件");
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("文件超过 64 KiB");
    }
    let text = String::from_utf8(bytes).context("文件不是 UTF-8 文本")?;
    if text.contains('\0') {
        bail!("文件包含二进制内容");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    #[test]
    fn noninteractive_confirmation_is_denied() {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            assert!(super::confirm("must not execute").is_err());
        }
    }
}
