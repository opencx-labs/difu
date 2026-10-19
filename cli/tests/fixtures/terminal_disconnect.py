"""Close real PTYs and reap every child, including a regressed busy-looping one."""
import os
import pty
import select
import signal
import sys
import tempfile
import time
from pathlib import Path


def exercise(mode, root):
    ready = root / mode
    env = dict(os.environ, DIFU_TEST_TERMINAL_MODE=mode,
               DIFU_TEST_TERMINAL_READY=str(ready))
    args = [sys.argv[1], "--exact", "terminal_input_child", "--nocapture"]
    pid, master = pty.fork()
    if pid == 0:
        if mode == "poll-no-hangup":
            # Multiplexers may close the input without delivering SIGHUP.
            signal.pthread_sigmask(signal.SIG_BLOCK, [signal.SIGHUP])
        os.execve(args[0], args, env)
    reaped = False
    try:
        deadline = time.monotonic() + 5
        while not ready.exists():
            found, status = os.waitpid(pid, os.WNOHANG)
            if found:
                reaped = True
                raise AssertionError(f"{mode}: child exited before input was ready: {status}")
            if time.monotonic() >= deadline:
                raise AssertionError(f"{mode}: input did not become ready")
            time.sleep(0.01)
        if mode == "ctrl-c":
            os.write(master, b"\x03")
        else:
            if mode == "cursor":
                # Close only after the query was written, so this tests the
                # response-reading loop rather than a failed stdout write.
                output = b""
                deadline = time.monotonic() + 5
                while b"\x1b[6n" not in output:
                    assert time.monotonic() < deadline, "Cursor query not sent"
                    if select.select([master], [], [], 0.05)[0]:
                        output += os.read(master, 4096)
            os.close(master)
            master = None
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            found, status = os.waitpid(pid, os.WNOHANG)
            if found:
                reaped = True
                assert os.waitstatus_to_exitcode(status) == 0, (mode, status)
                print(f"{mode}: exited normally")
                return
            time.sleep(0.01)
        raise AssertionError(f"{mode}: input never returned after terminal closure")
    finally:
        if master is not None:
            os.close(master)
        if not reaped:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)


with tempfile.TemporaryDirectory(prefix="difu-terminal-test-") as directory:
    for mode in ("ctrl-c", "poll", "read", "poll-no-hangup", "cursor"):
        exercise(mode, Path(directory))
