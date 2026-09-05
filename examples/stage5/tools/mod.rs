mod common;
mod list_directory;
mod read_file;
mod replace_text;
mod run_command;
mod search_text;

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;

// 工具只描述自身并执行操作，模型协议包装和分发集中在此处。
trait Tool {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters(&self) -> Value;
    fn execute(&self, root: &Path, args: &Value) -> Result<Value>;
}

fn registry() -> [&'static dyn Tool; 5] {
    [
        &list_directory::ListDirectory,
        &read_file::ReadFile,
        &search_text::SearchText,
        &replace_text::ReplaceText,
        &run_command::RunCommand,
    ]
}

pub fn definitions() -> Value {
    Value::Array(registry().iter().map(|tool| json!({
        "type": "function",
        "function": {"name": tool.name(), "description": tool.description(), "parameters": tool.parameters()}
    })).collect())
}

pub fn execute(root: &Path, name: &str, args: &Value) -> Result<Value> {
    for tool in registry() {
        if tool.name() == name {
            return tool.execute(root, args);
        }
    }
    bail!("未知工具：{name}")
}

#[cfg(test)]
mod tests {
    use super::common::MAX_BYTES;
    use super::*;
    use std::fs;
    #[test]
    fn registry_has_unique_names_and_matching_definitions() {
        let names = registry().map(|tool| tool.name());
        let unique = names.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), names.len());
        let definitions = definitions();
        for (definition, name) in definitions.as_array().unwrap().iter().zip(names) {
            assert_eq!(definition["function"]["name"], name);
        }
        assert!(execute(Path::new("."), "unknown", &json!({})).is_err());
    }
    #[test]
    fn tools_and_boundary() -> Result<()> {
        let root = std::env::temp_dir().join(format!("rust-agent-tools-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        fs::write(root.join("hello.rs"), "第一行\nfn hello() {}\n")?;
        assert_eq!(
            execute(&root, "read_file", &json!({"path":"hello.rs"}))?["text"],
            "第一行\nfn hello() {}\n"
        );
        assert_eq!(
            execute(&root, "search_text", &json!({"path":".","query":"hello"}))?["matches"][0]["line"],
            2
        );
        assert_eq!(
            execute(&root, "list_directory", &json!({"path":"."}))?["entries"][0],
            "hello.rs"
        );
        assert!(execute(&root, "read_file", &json!({"path":".."})).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.parent().unwrap(), root.join("outside"))?;
            assert!(execute(&root, "list_directory", &json!({"path":"outside"})).is_err());
        }
        fs::write(root.join("big"), vec![b'a'; MAX_BYTES as usize + 1])?;
        assert!(execute(&root, "read_file", &json!({"path":"big"})).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
