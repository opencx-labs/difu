#!/usr/bin/env python3
"""Pause worktree changes so CI can exercise startup and deletion progress."""
import os
import pathlib
import sys
import time

root = pathlib.Path(os.environ['DIFU_AGENT_FIXTURE'])
args = sys.argv[1:]
if any(a == 'worktree' and b == 'add' for a, b in zip(args, args[1:])):
    (root / 'worktree-started').touch()
    deadline = time.monotonic() + 15
    while (root / 'hold-worktree').exists():
        if time.monotonic() >= deadline:
            sys.exit('Fixture worktree was never released')
        time.sleep(0.02)
    if (root / 'fail-worktree').exists():
        sys.exit('Fixture worktree creation failed')
if any(a == 'worktree' and b == 'remove' for a, b in zip(args, args[1:])):
    deadline = time.monotonic() + 15
    while (root / 'hold-removal').exists():
        if time.monotonic() >= deadline:
            sys.exit('Fixture worktree removal was never released')
        time.sleep(0.02)
os.execv(os.environ['DIFU_REAL_GIT'], ['git', *args])
