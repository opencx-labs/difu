"""Drive the real Settings shell through a PTY without touching user state."""
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time

inputs = [b"\x1b[44;5u", b"\x1b[24~", b"\x1b[200~stop flashing\x1b[201~",
          b"\t", b"\x1b[B", b"\t", b"\r", b"\x10", b"\x13", b"\r", b"\x04",
          b"\x1b[Z", b"\x1b[A", b"\x1b[Z", b"\x1b[200~stop flashing\x1b[201~", b"\r",
          b"\x1b", b"\x1b[44;5u", b"\x1b[24~", b"\x1b"]
for width, height in [(42, 20), (140, 42)]:
    with tempfile.TemporaryDirectory(prefix="difu-settings-pty-") as directory:
        root = Path(directory)
        env = dict(os.environ, DIFU_SETTINGS_PTY_ROOT=directory)
        pid, master = pty.fork()
        if pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
            os.execve(sys.argv[1], [sys.argv[1], "--exact",
                "shell::settings_panel::terminal_tests::settings_terminal_child", "--nocapture"], env)
        output = b""
        reaped = False
        sent = 0
        deadline = time.monotonic() + 20
        try:
            while time.monotonic() < deadline:
                if select.select([master], [], [], .01)[0]:
                    try:
                        output += os.read(master, 65536)
                    except OSError:
                        pass
                phase = root / "phase"
                if sent < len(inputs) and phase.exists() and phase.read_text() == str(sent):
                    os.write(master, inputs[sent])
                    sent += 1
                found, status = os.waitpid(pid, os.WNOHANG)
                if found:
                    reaped = True
                    assert os.waitstatus_to_exitcode(status) == 0, (width, output.decode(errors="replace"))
                    assert (root / "restored").exists(), (width, "terminal not restored")
                    assert b"Settings" in output, (width, "panel not drawn")
                    assert b"\x1b[?1049h" in output and b"\x1b[?1049l" in output
                    break
            assert reaped, (width, "timed out", sent, output.decode(errors="replace"))
        finally:
            os.close(master)
            if not reaped:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
