use super::{
    Tool,
    common::{read_text, resolve},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;
pub(super) struct ReadFile;
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "读取工作目录内 UTF-8 文本文件，最大 64 KiB。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})
    }
    fn execute(&self, root: &Path, args: &Value) -> Result<Value> {
        let path = resolve(root, args["path"].as_str().context("缺少字符串参数 path")?)?;
        Ok(json!({"text":read_text(&path)?}))
    }
}
