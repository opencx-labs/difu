#!/usr/bin/env python3
import json, os, re, sys
from pathlib import Path
root = Path(os.environ['DIFU_TEST_FIXTURE'])
args = sys.argv[1:]
if args[0] == 'mcp':
    print(json.dumps([dict(name='fixture.server', enabled=True, transport={'type':'stdio'})]))
elif args[0] == 'app-server':
    for line in sys.stdin:
        request = json.loads(line)
        if request.get('method') == 'initialize':
            print(json.dumps(dict(id=request['id'], result={})), flush=True)
        elif request.get('method') == 'model/list':
            data = [dict(model='gpt-5.6-luna', displayName='Luna', supportedReasoningEfforts=[dict(reasoningEffort='high')]), dict(model='gpt-5.6-sol', displayName='Sol', supportedReasoningEfforts=[dict(reasoningEffort='low'), dict(reasoningEffort='high')])]
            print(json.dumps(dict(id=request['id'], result=dict(data=data, nextCursor=None))), flush=True)
else:
    assert any('"fixture.server" = { enabled = false, command = "false" }' in a for a in args)
    assert 'project_doc_max_bytes=0' in args
    assert any(a.startswith('developer_instructions=') for a in args)
    assert '--ignore-user-config' in args and '--ignore-rules' in args
    assert args[args.index('--sandbox')+1] == 'read-only'
    assert args[args.index('--model')+1] == 'gpt-5.6-luna'
    prompt = sys.stdin.read()
    match = re.search(r'Review input JSON file: (.+)', prompt)
    manifest = json.loads(Path(match.group(1)).read_text())
    assert Path('main.rs').read_text() == 'fn main() {\n    new();\n}\n'
    hunks = [h['id'] for f in manifest['snapshot']['files'] for h in f['hunks']]
    print(json.dumps(dict(type='turn.started')), flush=True)
    output = Path(args[args.index('--output-last-message')+1])
    output.write_text(json.dumps(dict(chapters=[dict(title='Use the new behavior', explanation='The entry point calls `new()` to select the new behavior.', hunks=hunks)])))
    with (root / 'turns').open('a') as f:
        f.write('turn\n')
