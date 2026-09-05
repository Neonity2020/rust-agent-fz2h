use super::{
    Tool,
    common::{MAX_BYTES, confirm, read_text, resolve},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub(super) struct ReplaceText;

fn write_if_unchanged(path: &Path, before: &str, after: &str) -> Result<()> {
    if read_text(path)? != before {
        bail!("确认期间文件已变化，未写入，请重新读取");
    }
    fs::write(path, after).context("写入文件失败")
}

fn replacement(before: &str, old: &str, new: &str) -> Result<String> {
    if old.is_empty() || before.matches(old).count() != 1 {
        bail!("old_text 必须非空且在文件中恰好出现一次，请重新读取文件并提供唯一上下文");
    }
    let after = before.replacen(old, new, 1);
    if after.len() as u64 > MAX_BYTES {
        bail!("修改后文件超过 64 KiB");
    }
    if new.contains('\0') {
        bail!("不能写入二进制内容");
    }
    Ok(after)
}

impl Tool for ReplaceText {
    fn name(&self) -> &'static str {
        "replace_text"
    }
    fn description(&self) -> &'static str {
        "精确替换现有文本文件的一处内容。先读取文件，old_text 必须唯一。展示修改并经用户确认后写入；拒绝后不要重复请求。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["path","old_text","new_text"],"additionalProperties":false})
    }
    fn execute(&self, root: &Path, args: &Value) -> Result<Value> {
        let path = resolve(root, args["path"].as_str().context("缺少 path")?)?;
        let old = args["old_text"].as_str().context("缺少 old_text")?;
        let new = args["new_text"].as_str().context("缺少 new_text")?;
        let before = read_text(&path)?;
        let after = replacement(&before, old, new)?;
        if before == after {
            return Ok(json!({"changed":false}));
        }
        let line = before[..before.find(old).unwrap()]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        let removed = old
            .split('\n')
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let added = new
            .split('\n')
            .map(|line| format!("+ {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        confirm(&format!(
            "修改文件：{}\n起始行：{line}\n{removed}\n{added}",
            path.display()
        ))?;
        write_if_unchanged(&path, &before, &after)?;
        Ok(json!({"changed":true,"path":path.strip_prefix(root)?.to_string_lossy()}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_replacement_requires_unique_context() {
        assert_eq!(replacement("中文 abc", "abc", "def").unwrap(), "中文 def");
        assert!(replacement("abc abc", "abc", "def").is_err());
        assert!(replacement("abc", "", "def").is_err());
        assert!(replacement("abc", "missing", "def").is_err());
    }
    #[test]
    fn writes_and_rejects_stale_contents() -> Result<()> {
        let path = std::env::temp_dir().join(format!("replace-test-{}", std::process::id()));
        fs::write(&path, "old")?;
        write_if_unchanged(&path, "old", "new")?;
        assert_eq!(fs::read_to_string(&path)?, "new");
        assert!(write_if_unchanged(&path, "old", "wrong").is_err());
        assert_eq!(fs::read_to_string(&path)?, "new");
        fs::remove_file(path)?;
        Ok(())
    }
}
