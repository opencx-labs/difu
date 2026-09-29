#!/usr/bin/env python3
"""Pause worktree changes so CI can exercise startup and deletion progress."""
import os
import pathlib
import subprocess
import sys
import time

root = pathlib.Path(os.environ['DIFU_AGENT_FIXTURE'])
args = sys.argv[1:]
if 'fetch' in args and (root / 'hold-fetch').exists():
    (root / 'fetch-started').touch()
    deadline = time.monotonic() + 15
    while (root / 'hold-fetch').exists():
        if time.monotonic() >= deadline:
            sys.exit('Fixture fetch was never released')
        time.sleep(0.02)
if any(a == 'worktree' and b == 'add' for a, b in zip(args, args[1:])):
    (root / 'worktree-started').touch()
    deadline = time.monotonic() + 15
    while (root / 'hold-worktree').exists():
        if time.monotonic() >= deadline:
            sys.exit('Fixture worktree was never released')
        time.sleep(0.02)
    if (root / 'fail-worktree').exists():
        sys.exit('Fixture worktree creation failed')
    if (root / 'hold-after-worktree').exists():
        subprocess.run([os.environ['DIFU_REAL_GIT'], *args], check=True)
        (root / 'worktree-created').touch()
        deadline = time.monotonic() + 15
        while (root / 'hold-after-worktree').exists():
            if time.monotonic() >= deadline:
                sys.exit('Fixture completed worktree was never released')
            time.sleep(0.02)
        sys.exit(0)
if any(a == 'worktree' and b == 'remove' for a, b in zip(args, args[1:])):
    deadline = time.monotonic() + 15
    while (root / 'hold-removal').exists():
        if time.monotonic() >= deadline:
            sys.exit('Fixture worktree removal was never released')
        time.sleep(0.02)
os.execv(os.environ['DIFU_REAL_GIT'], ['git', *args])
