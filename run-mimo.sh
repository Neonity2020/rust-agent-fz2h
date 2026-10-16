#!/bin/sh
# 以 MiMo Token Plan 线路启动 rust-agent：
#   ./run-mimo.sh                  # 交互模式
#   ./run-mimo.sh "问题"           # 单次问答
#   ./run-mimo.sh --resume         # 恢复会话
# 凭据在运行时从本机 mimocode 配置读取，不写入仓库；已设置的环境变量优先。
set -e

if [ ! -f Cargo.toml ]; then
    echo "请在 rust-agent 项目目录中运行本脚本（cargo run 需要 Cargo.toml）" >&2
    exit 1
fi

CONFIG="$HOME/.config/mimocode/mimocode.jsonc"

if [ -z "${RUST_AGENT_API_KEY:-}" ]; then
    if [ ! -f "$CONFIG" ]; then
        echo "找不到 $CONFIG，无法读取 MiMo 凭据；可先 export RUST_AGENT_API_KEY" >&2
        exit 1
    fi
    RUST_AGENT_API_KEY=$(python3 - "$CONFIG" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
match = re.search(r'"apiKey"\s*:\s*"([^"]+)"', text)
if not match:
    sys.exit("mimocode 配置中没有 apiKey")
print(match.group(1))
PY
)
    export RUST_AGENT_API_KEY
fi

export RUST_AGENT_ENDPOINT="${RUST_AGENT_ENDPOINT:-https://token-plan-cn.xiaomimimo.com/v1/chat/completions}"
export RUST_AGENT_MODEL="${RUST_AGENT_MODEL:-mimo-v2.6-pro}"
exec cargo run -- "$@"
