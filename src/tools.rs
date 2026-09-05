use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 200;

pub fn definitions() -> Value {
    json!([
        {"type":"function","function":{"name":"list_directory","description":"列出工作目录内一个目录的直接子项，最多 200 项。","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"read_file","description":"读取工作目录内 UTF-8 文本文件，最大 64 KiB。","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"search_text","description":"递归搜索目录中的字面文本，返回文件、行号和匹配行。跳过隐藏项、target 和 node_modules。","parameters":{"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"}},"required":["path","query"],"additionalProperties":false}}}
    ])
}

fn resolve(root: &Path, path: &str) -> Result<PathBuf> {
    let resolved = root
        .join(path)
        .canonicalize()
        .context("路径不存在或无法访问")?;
    if !resolved.starts_with(root) {
        bail!("路径超出工作目录");
    }
    Ok(resolved)
}

fn read_text(path: &Path) -> Result<String> {
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

pub fn execute(root: &Path, name: &str, args: &Value) -> Result<Value> {
    let path = resolve(root, args["path"].as_str().context("缺少字符串参数 path")?)?;
    match name {
        "read_file" => Ok(json!({"text":read_text(&path)?})),
        "list_directory" => {
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
        "search_text" => {
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
        _ => bail!("未知工具：{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
