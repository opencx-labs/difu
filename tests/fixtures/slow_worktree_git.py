#!/usr/bin/env python3
"""Pause worktree creation so CI can exercise input during session startup."""
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
os.execv(os.environ['DIFU_REAL_GIT'], ['git', *args])
