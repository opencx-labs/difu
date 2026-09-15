#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ['DIFU_TEST_FIXTURE'])
args = sys.argv[1:]
if 'fetch' in args:
    with (root / 'fetches.jsonl').open('a') as log:
        log.write(json.dumps(args) + '\n')
    assert '--atomic' in args and '--no-write-fetch-head' in args
    endpoint = args.index('--') + 1
    assert args[endpoint].startswith('https://github.com/')
    args[endpoint] = str(root / 'remote')
    assert all(':refs/difu/' in ref for ref in args[endpoint + 1:])
    os.environ['GIT_ALLOW_PROTOCOL'] = 'file'
os.execv(os.environ['DIFU_TEST_REAL_GIT'], [os.environ['DIFU_TEST_REAL_GIT'], *args])
