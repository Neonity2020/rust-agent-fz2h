use super::{
    Tool,
    common::{MAX_BYTES, confirm, read_text},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub(super) struct WriteFile;

fn target(root: &Path, name: &str) -> Result<PathBuf> {
    let relative = Path::new(name);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("写入路径必须是工作目录内的相对路径，不得包含 ..");
    }
    let path = root.join(relative);
    let mut ancestor = path.as_path();
    while !ancestor.exists() {
        if fs::symlink_metadata(ancestor).is_ok() {
            bail!("路径包含失效符号链接");
        }
        ancestor = ancestor.parent().context("无效路径")?;
    }
    if !ancestor.canonicalize()?.starts_with(root) {
        bail!("路径超出工作目录");
    }
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    Ok(path)
}

fn commit(path: &Path, before: Option<&str>, content: &str) -> Result<()> {
    match before {
        Some(before) if read_text(path)? != before => bail!("文件已变化，未覆盖"),
        None if fs::symlink_metadata(path).is_ok() => bail!("文件已出现，未覆盖"),
        _ => {}
    }
    let parent = path.parent().context("无效文件路径")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".rust-agent-write-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(content.as_bytes())?;
        if before.is_some() {
            file.set_permissions(fs::metadata(path)?.permissions())?;
        }
        file.sync_all()?;
        if before.is_none() {
            // hard_link 原子地拒绝覆盖并发创建的目标。
            fs::hard_link(&temp, path)?;
            fs::remove_file(&temp)?;
        } else {
            fs::rename(&temp, path)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "创建或完整覆盖 UTF-8 文件，自动创建父目录，最大 64 KiB。展示完整前后内容并确认后写入；小范围修改优先 replace_text。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false})
    }
    fn execute(&self, root: &Path, args: &Value) -> Result<Value> {
        let name = args["path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("缺少 path")?;
        let content = args["content"].as_str().context("缺少 content")?;
        if content.len() as u64 > MAX_BYTES || content.contains('\0') {
            bail!("内容必须是最多 64 KiB 的文本");
        }
        let path = target(root, name)?;
        let before = if path.exists() {
            Some(read_text(&path)?)
        } else {
            None
        };
        if before.as_deref() == Some(content) {
            return Ok(json!({"changed":false}));
        }
        let old = before
            .as_deref()
            .unwrap_or("")
            .split('\n')
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = content
            .split('\n')
            .map(|s| format!("+ {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        confirm(&format!(
            "{}文件：{}\n{old}\n{new}",
            if before.is_some() { "覆盖" } else { "创建" },
            path.display()
        ))?;
        if target(root, name)? != path {
            bail!("确认期间路径发生变化");
        }
        commit(&path, before.as_deref(), content)?;
        Ok(json!({"changed":true,"created":before.is_none(),"path":name}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn create_overwrite_and_boundaries() -> Result<()> {
        let root = std::env::temp_dir().join(format!("write-test-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let path = target(&root, "nested/test.rs")?;
        commit(&path, None, "hello")?;
        assert!(commit(&path, None, "wrong").is_err());
        commit(&path, Some("hello"), "world")?;
        assert!(commit(&path, Some("hello"), "wrong").is_err());
        assert_eq!(read_text(&path)?, "world");
        assert!(target(&root, "../outside").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.parent().unwrap(), root.join("escape"))?;
            assert!(target(&root, "escape/new.txt").is_err());
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
