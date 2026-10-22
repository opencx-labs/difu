#!/usr/bin/env python3
"""Exact native discovery/status fixture; fails closed on login or user turns."""
import json, os, sys, time
from pathlib import Path
root=Path(os.environ["DIFU_CONNECTIONS_FIXTURE"])
name = os.path.basename(sys.argv[0])
args = sys.argv[1:]
if 'status' in args:
    if (root/'hang-status').exists(): time.sleep(30)
    if (root/'malformed-status').exists(): print('private@example.invalid sk-secret');sys.exit(2)
    if (root/'signed-out').exists():
        if name=='claude': print('{"loggedIn":false}');sys.exit(1)
        print('Not logged in',file=sys.stderr);sys.exit(1)
else:
    if (root/'fail-models').exists(): sys.exit(2)
    if name=='codex' and (root/'slow-codex').exists():
        (root/'codex-started').write_text('ready')
        time.sleep(3)

if name == 'codex' and args == ['login', 'status']:
    print('WARNING: PATH unavailable\nLogged in using ChatGPT', file=sys.stderr)
    sys.exit(0)
if name == 'claude' and args == ['auth', 'status', '--json']:
    print(json.dumps(dict(loggedIn=True, email='private@example.invalid')))
    sys.exit(0)
if name == 'codex':
    assert args == ['app-server'], args
else:
    assert args == ['--print','--verbose','--input-format','stream-json','--output-format','stream-json','--include-partial-messages','--replay-user-messages','--permission-prompt-tool','stdio','--no-session-persistence','--setting-sources=','--strict-mcp-config','--mcp-config','{"mcpServers":{}}'], args
for line in sys.stdin:
    frame = json.loads(line)
    if name == 'codex':
        method = frame['method']
        if method == 'initialized': continue
        if method == 'initialize':
            assert frame['params']['capabilities']=={'experimentalApi':True},frame
            result = {}
        elif method == 'model/list':
            assert frame['params'] == dict(limit=100,includeHidden=True,cursor=None)
            result = dict(data=[dict(model='gpt-fixture',displayName='Fixture',supportedReasoningEfforts=[dict(reasoningEffort='high')])],nextCursor=None)
        elif method == 'collaborationMode/list':
            assert frame['params'] == {}
            result = dict(data=[dict(mode='default'),dict(mode='plan')])
        else: raise AssertionError(method)
        print(json.dumps(dict(id=frame['id'], result=result)), flush=True)
    else:
        assert frame['type'] == 'control_request', frame
        assert frame['request'] == dict(subtype='initialize'), frame
        print(json.dumps(dict(type='control_response',response=dict(subtype='success',request_id=frame['request_id'],response=dict(models=[dict(value='sonnet',displayName='Sonnet',supportedEffortLevels=['low','high'])])))),flush=True)
