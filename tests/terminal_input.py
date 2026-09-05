"""macOS/Linux PTY regression: cargo build && python3 tests/terminal_input.py."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import tempfile
import termios
import time


def check(text, keys):
    with tempfile.TemporaryDirectory() as directory:
        auth = Path(directory) / "opencode/auth.json"
        auth.parent.mkdir()
        auth.write_text(json.dumps({"opencode-go": {"type": "api", "key": "test-only"}}))
        pid, fd = pty.fork()
        if pid == 0:
            os.environ["XDG_DATA_HOME"] = directory
            os.environ["TERM"] = "xterm-256color"
            os.execv("target/debug/rust-agent", ["rust-agent"])
        output = bytearray()
        def read_for(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([fd], [], [], 0.05)[0]:
                    try:
                        chunk = os.read(fd, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            return
                        raise
                    if not chunk:
                        return
                    output.extend(chunk)
                    # Answer the editor's cursor-position query like a terminal.
                    if b"\x1b[6n" in chunk:
                        os.write(fd, b"\x1b[1;1R")
        reaped = False
        try:
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 20, 0, 0))
            read_for(0.5)
            os.write(fd, text.encode())
            read_for(0.2)
            os.write(fd, keys)
            read_for(0.2)
            os.write(fd, b"/exit\r")
            read_for(1)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                finished, status = os.waitpid(pid, os.WNOHANG)
                if finished == pid:
                    reaped = True
                    break
                time.sleep(0.01)
            assert reaped, repr(bytes(output))
            assert os.waitstatus_to_exitcode(status) == 0
            assert "正在请求".encode() not in output, "Unexpected model request"
            assert b"\x1b[" in output, "Editor did not activate terminal rendering"
        finally:
            if not reaped:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(fd)


check("中文测试", b"\x7f" * 4)
check("中文abcdefgh中文abcdefgh", b"\x7f" * 20)
check("e\u0301你好", b"\x7f" * 3)
check("取消这段文字", b"\x03")
print("PASS: Chinese, wrapped mixed text, combining character deletion, Ctrl-C")
