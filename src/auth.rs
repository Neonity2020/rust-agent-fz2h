use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::env;

pub(crate) fn load_api_key() -> Result<String> {
    let data_dir = match env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        Some(path) => std::path::PathBuf::from(path),
        None => std::path::PathBuf::from(env::var_os("HOME").context("无法确定用户主目录")?)
            .join(".local/share"),
    };
    let path = data_dir.join("opencode/auth.json");
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("无法读取 {}，请先在 OpenCode 中登录 Go", path.display()))?;
    let auth: Value = serde_json::from_str(&contents).context("OpenCode auth.json 格式无效")?;
    let go = &auth["opencode-go"];
    if go["type"].as_str() != Some("api") {
        bail!("auth.json 缺少 Go API 凭据，请先在 OpenCode 中连接 OpenCode Go");
    }
    let key = go["key"]
        .as_str()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .context("auth.json 中的 Go API Key 缺失或为空")?;
    Ok(key.to_owned())
}
