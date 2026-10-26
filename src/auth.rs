use anyhow::{Result, bail};
use std::env;

/// 读取 API Key：先看通用覆盖变量，再看 DeepSeek 官方变量。
/// 凭据只从环境读取，程序不落盘、不打印密钥。
pub(crate) fn load_api_key() -> Result<String> {
    for name in ["RUST_AGENT_API_KEY", "DEEPSEEK_API_KEY"] {
        if let Some(value) = env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            return Ok(value);
        }
    }
    bail!(
        "未找到 API Key：请设置环境变量 DEEPSEEK_API_KEY（或 RUST_AGENT_API_KEY），\
         密钥可在 https://platform.deepseek.com/api_keys 申请"
    );
}
