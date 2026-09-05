use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub(super) const MAX_BYTES: u64 = 64 * 1024;
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
