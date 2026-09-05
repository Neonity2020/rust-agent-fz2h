use super::{Tool, common::resolve};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
const MAX_ENTRIES: usize = 200;
pub(super) struct ListDirectory;
impl Tool for ListDirectory {
    fn name(&self) -> &'static str {
        "list_directory"
    }
    fn description(&self) -> &'static str {
        "列出工作目录内一个目录的直接子项，最多 200 项。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})
    }
    fn execute(&self, root: &Path, args: &Value) -> Result<Value> {
        let path = resolve(root, args["path"].as_str().context("缺少字符串参数 path")?)?;
        let entries = fs::read_dir(path)?
            .take(MAX_ENTRIES + 1)
            .collect::<std::io::Result<Vec<_>>>()?;
        let truncated = entries.len() > MAX_ENTRIES;
        let mut names = entries
            .into_iter()
            .take(MAX_ENTRIES)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        Ok(json!({"entries":names,"truncated":truncated}))
    }
}
