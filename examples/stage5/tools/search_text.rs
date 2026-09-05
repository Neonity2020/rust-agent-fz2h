use super::{
    Tool,
    common::{read_text, resolve},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
pub(super) struct SearchText;
impl Tool for SearchText {
    fn name(&self) -> &'static str {
        "search_text"
    }
    fn description(&self) -> &'static str {
        "递归搜索目录中的字面文本，返回文件、行号和匹配行。跳过隐藏项、target 和 node_modules。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"}},"required":["path","query"],"additionalProperties":false})
    }
    fn execute(&self, root: &Path, args: &Value) -> Result<Value> {
        let path = resolve(root, args["path"].as_str().context("缺少字符串参数 path")?)?;
        let query = args["query"]
            .as_str()
            .filter(|q| !q.is_empty())
            .context("query 必须为非空字符串")?;
        let mut pending = vec![path];
        let mut matches = Vec::new();
        let mut visited = 0;
        let mut skipped = 0;
        let mut budget_exhausted = false;
        while let Some(path) = pending.pop() {
            visited += 1;
            if visited > 2000 || matches.len() >= 50 {
                budget_exhausted = true;
                break;
            }
            if path.is_dir() {
                let entries = match fs::read_dir(&path) {
                    Ok(e) => e,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                for (index, entry) in entries.take(2001).enumerate() {
                    if index == 2000 {
                        budget_exhausted = true;
                        break;
                    }
                    if pending.len() >= 2000 {
                        budget_exhausted = true;
                        break;
                    }
                    let Ok(entry) = entry else {
                        skipped += 1;
                        continue;
                    };
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with('.') || name == "target" || name == "node_modules" {
                        continue;
                    }
                    // 不跟随搜索中遇到的符号链接，避免越界和目录循环。
                    if entry.file_type()?.is_symlink() {
                        continue;
                    }
                    pending.push(entry.path());
                }
            } else {
                let Ok(text) = read_text(&path) else {
                    skipped += 1;
                    continue;
                };
                for (line, text) in text.lines().enumerate() {
                    if text.contains(query) {
                        matches.push(json!({"path":path.strip_prefix(root)?.to_string_lossy(),"line":line+1,"text":text.chars().take(300).collect::<String>()}));
                        if matches.len() == 50 {
                            budget_exhausted = true;
                            break;
                        }
                    }
                }
            }
        }
        Ok(json!({"matches":matches,"truncated":budget_exhausted,"skipped_files":skipped}))
    }
}
