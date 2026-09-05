use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::Path,
};

const MAX_SESSION_BYTES: u64 = 4 * 1024 * 1024;

pub fn save(path: &Path, root: &Path, id: &str, messages: &[Value]) -> Result<()> {
    validate(messages)?;
    let bytes = serde_json::to_vec(&json!({"version":1,"root":root,"id":id,"messages":messages}))?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        bail!("会话超过 4 MiB，未保存");
    }
    fs::create_dir_all(path.parent().context("无效会话路径")?)?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).context("创建会话临时文件失败")?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub fn load(path: &Path, root: &Path) -> Result<(String, Vec<Value>)> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .context("没有可恢复的会话，请先使用 /save")?
        .take(MAX_SESSION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        bail!("会话超过 4 MiB");
    }
    let data: Value = serde_json::from_slice(&bytes).context("会话文件损坏")?;
    if data["version"] != 1 || data["root"] != json!(root) {
        bail!("会话版本或工作目录不匹配");
    }
    let id = data["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("会话缺少 id")?;
    let messages = data["messages"].as_array().context("会话缺少消息列表")?;
    validate(messages)?;
    Ok((id.to_owned(), messages.clone()))
}

fn validate(messages: &[Value]) -> Result<()> {
    if messages.first().and_then(|m| m["role"].as_str()) != Some("system") {
        bail!("会话缺少系统消息");
    }
    let mut pending = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        let role = message["role"].as_str().context("无效消息角色")?;
        if role != "tool" && !pending.is_empty() {
            bail!("会话工具结果不完整");
        }
        match role {
            "system" if index == 0 => {}
            "user" => {}
            "assistant" => {
                if let Some(calls) = message["tool_calls"].as_array() {
                    for call in calls {
                        let id = call["id"].as_str().context("工具缺少 id")?;
                        if !pending.insert(id.to_owned()) {
                            bail!("重复工具 id");
                        }
                    }
                }
            }
            "tool" => {
                if !message["tool_call_id"]
                    .as_str()
                    .is_some_and(|id| pending.remove(id))
                {
                    bail!("工具结果没有对应调用");
                }
            }
            _ => bail!("无效消息角色"),
        }
    }
    if !pending.is_empty() {
        bail!("会话工具结果不完整");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_invalid_files() -> Result<()> {
        let root = std::env::temp_dir().join(format!("session-test-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let path = root.join("session.json");
        let messages = vec![
            json!({"role":"system","content":"test"}),
            json!({"role":"user","content":"中文"}),
            json!({"role":"assistant","content":"你好"}),
        ];
        save(&path, &root, "test-id", &messages)?;
        assert_eq!(load(&path, &root)?, ("test-id".into(), messages));
        assert!(load(&path, Path::new("/wrong")).is_err());
        fs::write(&path, "broken")?;
        assert!(load(&path, &root).is_err());
        assert!(
            validate(&[
                json!({"role":"system"}),
                json!({"role":"tool","tool_call_id":"missing"})
            ])
            .is_err()
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
