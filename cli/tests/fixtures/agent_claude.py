#!/usr/bin/env python3
"""Claude stream/control fixture. Never starts a model or validates a project."""
import json, os, pathlib, subprocess, sys, uuid
root = pathlib.Path(os.environ['DIFU_AGENT_FIXTURE'])
if 'auto-mode' in sys.argv:
    print(json.dumps(dict(allow=[],soft_deny=[],hard_deny=[],environment=[])),flush=True)
    sys.exit(0)
if (root/'reject-settings').exists() and '--settings' in sys.argv:
    print('Fixture native settings reload rejected',file=sys.stderr)
    sys.exit(2)
connection = uuid.uuid4().hex[:8]
mcp_enabled = True
turn = 0
pending = None
pending_permission = False
initializing = None
session = 'fixture-claude'
permission_mode = next((arg.split('=',1)[1] for arg in sys.argv if arg.startswith('--permission-mode=')), 'bypassPermissions' if (root/'claude-inherited-bypass').exists() else 'default')
if '--permission-mode' in sys.argv: permission_mode = sys.argv[sys.argv.index('--permission-mode')+1]
with (root/'claude-starts.jsonl').open('a') as log:
    log.write(json.dumps(dict(args=sys.argv[1:], cwd=os.getcwd()))+'\n')
resume_id = next((arg.split('=',1)[1] for arg in sys.argv if arg.startswith('--resume=')),None)
if resume_id and (not (root/'claude-conversation').exists() or resume_id != session):
    print(f'No conversation found with session ID: {resume_id}',file=sys.stderr,flush=True)
    sys.exit(2)
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
            if '{"disableAllHooks":true}' in sys.argv:
                reply(frame, dict(commands=[
                    dict(name='fixture-skill', description='Fixture\n  skill (project)', argumentHint='<target>'),
                    dict(name='compact', description='Compact the conversation', argumentHint=''),
                    dict(name='__server-only', description='Server-launched sessions only', argumentHint=''),
                ]))
            elif '--no-session-persistence' in sys.argv:
                reply(frame, dict(models=[dict(value='sonnet', displayName='Sonnet', supportedEffortLevels=['low','medium','high'])]))
            else:
                initializing = frame
                emit(dict(type='control_request', request_id='difu-tools', request=dict(subtype='mcp_message', server_name='difu', message=dict(jsonrpc='2.0', id=1, method='tools/list'))))
        elif request['subtype'] == 'set_permission_mode':
            if request['mode'] == 'auto' and (root/'transient-auto').exists():
                (root/'transient-auto').unlink()
                emit(dict(type='control_response',response=dict(subtype='error',request_id=frame['request_id'],error='Transient internal transport failure',error_code='internal_error')))
            elif request['mode'] == 'auto' and (root/'reject-auto').exists():
                emit(dict(type='control_response',response=dict(subtype='error',request_id=frame['request_id'],error='Auto mode unavailable for this model',error_code='auto_mode_model')))
            elif request['mode'] == 'bypassPermissions' and '--allow-dangerously-skip-permissions' not in sys.argv:
                emit(dict(type='control_response',response=dict(subtype='error',request_id=frame['request_id'],error='Bypass not enabled at launch',error_code='bypass_not_launched')))
            else:
                permission_mode = request['mode']
                reply(frame,dict(mode=permission_mode))
        elif request['subtype'] == 'mcp_status':
            if (root/'mcp-interleave').exists():
                (root/'mcp-interleave').unlink()
                pending = 'mcp-interleaved-approval'
                pending_permission = True
                emit(dict(type='control_request', request_id=pending, request=dict(subtype='can_use_tool',tool_name='Bash',input=dict(command='echo preserved approval'))))
            reply(frame, {'mcpServers':[{'name':'fixture','status':'connected' if mcp_enabled else 'disabled','scope':'project','config':{'type':'http','url':'https://fixture.example.com'},'tools':[{'name':'hello'}] if mcp_enabled else []}]})
        elif request['subtype'] == 'mcp_toggle':
            assert request['serverName'] == 'fixture'
            mcp_enabled = request['enabled']
            reply(frame,{})
        elif request['subtype'] == 'mcp_reconnect':
            assert request['serverName'] == 'fixture' and mcp_enabled
            reply(frame,{})
        elif request['subtype'] == 'mcp_authenticate': reply(frame,{'authUrl':'https://auth.example.com/login'})
        elif request['subtype'] == 'get_usage':
            if (root/'usage-interleave').exists():
                (root/'usage-interleave').unlink()
                pending = 'usage-interleaved-approval'
                pending_permission = True
                emit(dict(type='control_request',request_id=pending,request=dict(subtype='can_use_tool',tool_name='Bash',input=dict(command='SECRET_FIXTURE_ARG'))))
            reply(frame, dict(subscription_type='max', rate_limits_available=True, rate_limits=dict(five_hour=dict(utilization=40, resets_at='2027-01-15T08:00:00Z')), session=dict(total_cost_usd=1.25)))
        elif request['subtype'] == 'interrupt':
            if pending and pending_permission:
                cancelled = pending
                emit(dict(type='control_cancel_request',request_id=cancelled))
                reply(frame,{})
                pending = None
                pending_permission = False
                emit(dict(type='result',subtype='error_during_execution',is_error=True,errors=['[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use'],permission_denials=[dict(tool_name='Bash',tool_use_id=cancelled,tool_input={})]))
            else:
                reply(frame, {})
                pending = None
                pending_permission = False
                state('idle')
                emit(dict(type='result', subtype='success', is_error=False))
        else:
            raise AssertionError(request)
    elif frame['type'] == 'user':
        (root/'claude-conversation').write_text(session)
        if (root/'drop-claude-before-ack').exists():
            (root/'drop-claude-before-ack').unlink()
            sys.exit(0)
        turn += 1
        state('running')
        emit(dict(type='system', subtype='init', model='sonnet', permissionMode=permission_mode))
        emit(frame) # --replay-user-messages acknowledges exactly the client's UUID.
        prompt = '\n'.join(block.get('text','') for block in frame['message']['content'] if not block.get('text','').startswith('Difu conversation handoff.'))
        if prompt.startswith('claude register worktree: '):
            pending = 'register-worktree'
            emit(dict(type='control_request', request_id=pending, request=dict(subtype='mcp_message', server_name='difu', message=dict(jsonrpc='2.0', id=2, method='tools/call', params=dict(name='difu_register_worktree', arguments=dict(path=prompt.split(': ',1)[1]))))))
        elif prompt.startswith('claude peer: '):
            emit(dict(type='control_request', request_id='peer-call', request=dict(subtype='mcp_message', server_name='difu', message=dict(jsonrpc='2.0', id=4, method='tools/call', params=dict(name='difu_peer_sessions', arguments=json.loads(prompt.split(': ', 1)[1]))))))
        elif prompt == 'delete this session':
            pathlib.Path('discard.txt').write_text('dirty self-deletion fixture')
            pending = 'delete-session'
            emit(dict(type='control_request', request_id=pending, request=dict(subtype='mcp_message', server_name='difu', message=dict(jsonrpc='2.0', id=3, method='tools/call', params=dict(name='difu_delete_session', arguments={})))))
        elif prompt.startswith('fixture shell: '):
            subprocess.Popen([sys.executable, prompt.split(': ', 1)[1]], start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            complete()
        elif prompt == 'claude multi question':
            tool = f'ask-{connection}-{turn}'
            ask = dict(questions=[dict(question='Which checks?', header='Checks', multiSelect=True, options=[dict(label=label, description='') for label in ('Lint', 'Tests', 'Build')])])
            emit(dict(type='assistant', message=dict(id=f'assistant-{connection}-{turn}', content=[dict(type='tool_use', id=tool, name='AskUserQuestion', input=ask)])))
            pending = f'permission-{tool}'
            pending_permission = True
            emit(dict(type='control_request', request_id=f'permission-{tool}', request=dict(subtype='can_use_tool', tool_name='AskUserQuestion', input=ask, tool_use_id=tool)))
        elif prompt == 'claude plan exit':
            pending = 'exit-plan'
            pending_permission = True
            emit(dict(type='control_request',request_id=pending,request=dict(subtype='can_use_tool',tool_name='ExitPlanMode',input=dict(plan='# Test plan\n\nKeep every existing file.\n',planFilePath='/tmp/fixture-plan.md'))))
        elif 'claude tool' in prompt:
            tool = f'tool-{connection}-{turn}'
            emit(dict(type='assistant', message=dict(id=f'assistant-{connection}-{turn}', content=[dict(type='tool_use', id=tool, name='Bash', input=dict(command='echo fixture'))])))
            pending = tool
            pending_permission = True
            emit(dict(type='control_request', request_id=tool, request=dict(subtype='can_use_tool', tool_name='Bash', input=dict(command='echo fixture'))))
        elif prompt == 'claude fatal after cancellation':
            state('idle')
            emit(dict(type='result',subtype='error_during_execution',is_error=True,errors=['Fatal provider failure after cancellation']))
        elif 'claude wait' in prompt:
            pass
        else:
            pathlib.Path('claude.txt').write_text('Claude worktree edit\n')
            complete()
    elif frame['type'] == 'control_response':
        if frame['response']['request_id'] == 'difu-tools':
            tools = frame['response']['response']['mcp_response']['result']['tools']
            assert tools[0]['name'] == 'difu_present_artifact' and 'inputSchema' in tools[0]
            assert any(tool['name'] == 'difu_remove_questions' for tool in tools)
            reply(initializing, dict(current_permission_mode=permission_mode, models=[dict(value='sonnet', displayName='Sonnet', supportedEffortLevels=['high'])]))
            initializing = None
            emit(dict(type='system',subtype='session_state_changed',state='idle',permissionMode=permission_mode))
            continue
        if frame['response']['request_id'] in ('register-worktree', 'delete-session'):
            assert not frame['response']['response']['mcp_response']['result']['isError']
            pending = None
            pending_permission = False
            continue
        if frame['response']['request_id'] == 'peer-call':
            complete()
            continue
        assert frame['response']['request_id'] == pending
        response = frame['response']['response']
        allowed = response['behavior'] == 'allow'
        if pending == 'exit-plan':
            for change in response.get('updatedPermissions',[]):
                if change.get('type') == 'setMode':
                    assert change['destination'] == 'session'
                    permission_mode = change['mode']
            emit(dict(type='system',subtype='status',permissionMode=permission_mode))
        tool = pending.removeprefix('permission-')
        emit(dict(type='user', message=dict(role='user', content=[dict(type='tool_result', tool_use_id=tool, content='Allowed' if allowed else response.get('message', 'Denied'), is_error=not allowed)])))
        pending = None
        pending_permission = False
        complete()
    else:
        raise AssertionError(frame)
