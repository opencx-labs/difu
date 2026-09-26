#!/usr/bin/env python3
"""Claude stream/control fixture. Never starts a model or validates a project."""
import json, os, pathlib, sys, uuid
root = pathlib.Path(os.environ['DIFU_AGENT_FIXTURE'])
connection = uuid.uuid4().hex[:8]
turn = 0
pending = None
initializing = None
session = 'fixture-claude'
with (root/'claude-starts.jsonl').open('a') as log:
    log.write(json.dumps(dict(args=sys.argv[1:], cwd=os.getcwd()))+'\n')
def emit(frame):
    frame.setdefault('session_id', session)
    print(json.dumps(frame), flush=True)
def state(value):
    emit(dict(type='system', subtype='session_state_changed', state=value, sdk_host_only=True))
def reply(request, response):
    emit(dict(type='control_response', response=dict(subtype='success', request_id=request['request_id'], response=response)))
def complete():
    message_id = f'message-{connection}-{turn}'
    emit(dict(type='stream_event', event=dict(type='message_start', message=dict(id=message_id))))
    emit(dict(type='stream_event', event=dict(type='content_block_start', index=0, content_block=dict(type='text', text=''))))
    emit(dict(type='stream_event', event=dict(type='content_block_delta', index=0, delta=dict(type='text_delta', text='Finished Claude task'))))
    emit(dict(type='assistant', message=dict(id=message_id, content=[dict(type='text', text='Finished Claude task')])) )
    state('idle')
    emit(dict(type='result', subtype='success', is_error=False, result='Finished Claude task', usage=dict(input_tokens=10, output_tokens=4)))
for line in sys.stdin:
    frame = json.loads(line)
    with (root/'claude-protocol.jsonl').open('a') as log:
        log.write(json.dumps(frame)+'\n')
    if frame['type'] == 'control_request':
        request = frame['request']
        if request['subtype'] == 'initialize':
            if '--no-session-persistence' in sys.argv:
                reply(frame, dict(models=[dict(value='sonnet', displayName='Sonnet', supportedEffortLevels=['low','medium','high'])]))
            else:
                initializing = frame
                emit(dict(type='control_request', request_id='difu-tools', request=dict(subtype='mcp_message', server_name='difu', message=dict(jsonrpc='2.0', id=1, method='tools/list'))))
        elif request['subtype'] == 'get_usage':
            reply(frame, dict(subscription_type='max', rate_limits_available=True, rate_limits=dict(five_hour=dict(utilization=40, resets_at='2027-01-15T08:00:00Z')), session=dict(total_cost_usd=1.25)))
        elif request['subtype'] == 'interrupt':
            reply(frame, {})
            pending = None
            state('idle')
            emit(dict(type='result', subtype='success', is_error=False))
        else:
            raise AssertionError(request)
    elif frame['type'] == 'user':
        turn += 1
        state('running')
        emit(dict(type='system', subtype='init', model='sonnet', permissionMode='default'))
        emit(frame) # --replay-user-messages acknowledges exactly the client's UUID.
        prompt = '\n'.join(block.get('text','') for block in frame['message']['content'] if not block.get('text','').startswith('Difu conversation handoff.'))
        if prompt.startswith('claude register worktree: '):
            pending = 'register-worktree'
            emit(dict(type='control_request', request_id=pending, request=dict(subtype='mcp_message', server_name='difu', message=dict(jsonrpc='2.0', id=2, method='tools/call', params=dict(name='difu_register_worktree', arguments=dict(path=prompt.split(': ',1)[1]))))))
        elif 'claude tool' in prompt:
            tool = f'tool-{connection}-{turn}'
            emit(dict(type='assistant', message=dict(id=f'assistant-{connection}-{turn}', content=[dict(type='tool_use', id=tool, name='Bash', input=dict(command='echo fixture'))])))
            pending = tool
            emit(dict(type='control_request', request_id=tool, request=dict(subtype='can_use_tool', tool_name='Bash', input=dict(command='echo fixture'))))
        elif 'claude wait' in prompt:
            pass
        else:
            pathlib.Path('claude.txt').write_text('Claude worktree edit\n')
            complete()
    elif frame['type'] == 'control_response':
        if frame['response']['request_id'] == 'difu-tools':
            tools = frame['response']['response']['mcp_response']['result']['tools']
            assert tools[0]['name'] == 'difu_present_artifact' and 'inputSchema' in tools[0]
            reply(initializing, dict(models=[dict(value='sonnet', displayName='Sonnet', supportedEffortLevels=['high'])]))
            initializing = None
            continue
        if frame['response']['request_id'] == 'register-worktree':
            assert not frame['response']['response']['mcp_response']['result']['isError']
            continue
        assert frame['response']['request_id'] == pending
        allowed = frame['response']['response']['behavior'] == 'allow'
        emit(dict(type='user', message=dict(role='user', content=[dict(type='tool_result', tool_use_id=pending, content='Allowed' if allowed else 'Denied', is_error=not allowed)])))
        pending = None
        complete()
    else:
        raise AssertionError(frame)
