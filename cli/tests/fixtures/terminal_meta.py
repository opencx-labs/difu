"""Send raw bytes through a real PTY into Crossterm's public poll/read API."""
import os
import pty
import signal
import sys
import tempfile
import time
from pathlib import Path


def exercise(name, chunks, expected, root):
    ready, result = root / (name + '.ready'), root / (name + '.result')
    prefix_sent, prefix_read = root / (name + '.sent'), root / (name + '.read')
    env = dict(os.environ, DIFU_TEST_META_EXPECTED=expected,
               DIFU_TEST_META_READY=str(ready), DIFU_TEST_META_RESULT=str(result))
    if len(chunks) > 1:
        env['DIFU_TEST_META_PREFIX_READ'] = str(prefix_read)
        if expected != 'esc-then-up':
            env['DIFU_TEST_META_PREFIX_SENT'] = str(prefix_sent)
    args = [sys.argv[1], '--exact', 'terminal_meta_child', '--nocapture']
    pid, master = pty.fork()
    if pid == 0:
        os.execve(args[0], args, env)
    reaped = False
    try:
        deadline = time.monotonic() + 4
        while not ready.exists():
            found, status = os.waitpid(pid, os.WNOHANG)
            if found:
                reaped = True
                raise AssertionError(f'{name}: exited before ready: {status}')
            assert time.monotonic() < deadline, f'{name}: no readiness'
            time.sleep(.001)
        os.write(master, chunks[0])
        if len(chunks) > 1:
            prefix_sent.write_text('sent')
            while not prefix_read.exists() and (expected == 'esc-then-up' or not result.exists()):
                assert time.monotonic() < deadline, f'{name}: prefix not processed'
                time.sleep(.001)
            if expected == 'esc-then-up' or not result.exists():
                os.write(master, chunks[1])
        while time.monotonic() < deadline:
            found, status = os.waitpid(pid, os.WNOHANG)
            if found:
                reaped = True
                value = result.read_text() if result.exists() else '(no event)'
                assert os.waitstatus_to_exitcode(status) == 0, f'{name}: {value}'
                print(f'{name}: {value}')
                return
            time.sleep(.001)
        raise AssertionError(f'{name}: timed out')
    finally:
        os.close(master)
        if not reaped:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)


cases = [
    ('shift-left-csi', [b'\x1b[1;2D'], 'shift-left'),
    ('ctrl-enter-csi-u', [b'\x1b[13;5u'], 'ctrl-enter'),
    ('shift-tab-csi', [b'\x1b[Z'], 'backtab'),
    ('meta-csi-up', [b'\x1b\x1b[A'], 'alt-up'),
    ('meta-ss3-up', [b'\x1b\x1bOA'], 'alt-up'),
    ('meta-csi-down', [b'\x1b\x1b[B'], 'alt-down'),
    ('modified-csi-up', [b'\x1b[1;3A'], 'alt-up'),
    ('ordinary-up', [b'\x1b[A'], 'up'),
    ('real-escape', [b'\x1b'], 'esc'),
    ('real-escape-then-up', [b'\x1b', b'\x1b[A'], 'esc-then-up'),
    ('split-meta-prefix', [b'\x1b\x1b', b'[A'], 'alt-up'),
    ('split-first-escape', [b'\x1b', b'\x1b[A'], 'alt-up'),
    ('split-meta-ss3', [b'\x1b\x1b', b'OA'], 'alt-up'),
    ('split-csi-introducer', [b'\x1b\x1b[', b'A'], 'alt-up'),
    ('split-ss3-introducer', [b'\x1b\x1bO', b'A'], 'alt-up'),
]
with tempfile.TemporaryDirectory(prefix='difu-meta-') as directory:
    failures = []
    for name, chunks, expected in cases:
        try:
            exercise(name, chunks, expected, Path(directory))
        except AssertionError as error:
            failures.append(str(error))
    assert not failures, '\n'.join(failures)
