#!/usr/bin/env python3
"""Deterministic app-server fixture: never runs a model or project validation."""
import json, os, pathlib, subprocess, sys, uuid
root = pathlib.Path(os.environ['DIFU_AGENT_FIXTURE'])
if sys.argv[1:3] == ['mcp', 'list']:
    print('[]')
    sys.exit(0)
if sys.argv[1:2] == ['exec']:
    prompt = sys.stdin.read()
    schema = json.loads(pathlib.Path(sys.argv[sys.argv.index('--output-schema')+1]).read_text())
    suggestion = 'suggestion' in schema['properties']
    with (root/('suggestions.jsonl' if suggestion else 'titles.jsonl')).open('a') as log:
        log.write(json.dumps({'args':sys.argv[1:], 'input':json.loads(prompt)})+'\n')
    output = pathlib.Path(sys.argv[sys.argv.index('--output-last-message')+1])
    output.write_text(json.dumps({'suggestion':'Okay, implement the plan.'} if suggestion else {'title':'Fixture coding session'}))
    sys.exit(0)
mcp_enabled = True
turn = 0
connection = uuid.uuid4().hex[:8]
sandbox = 'workspace-write'
workspace_policy = dict(networkAccess=True,writableRoots=['/fixture-extra']) if (root/'custom-permissions').exists() else {}
approval_policy = 'on-request'
approvals_reviewer = 'auto_review' if (root/'inherit-auto-reviewer').exists() else 'user'
collaboration_mode = dict(mode='default',settings=dict(model='fixture-model',reasoning_effort='high',developer_instructions=None))
active = None
loaded = False
current_model = 'fixture-model'
current_effort = 'high'
thread_id = 'fixture-thread'
def emit(value):
    print(json.dumps(value), flush=True)
def reply(request, result):
    emit({'id':request['id'], 'result':result})
def event(method, params):
    emit({'method':method, 'params':params})
def complete():
    global active
    event('item/completed', {'threadId':thread_id,'turnId':active,'item':{'id':'message-'+connection+'-'+str(turn),'type':'agentMessage','text':'Finished fixture task'}})
    event('turn/completed', {'threadId':thread_id,'turn':{'id':active,'status':'completed'}})
    active = None
for line in sys.stdin:
    request = json.loads(line)
    with (root/'protocol.jsonl').open('a') as log:
        log.write(json.dumps(request)+'\n')
    method = request.get('method')
    params = request.get('params',{})
    if method == 'initialize': reply(request, {'userAgent':'fixture'})
    elif method == 'initialized': pass
    elif method == 'account/rateLimits/read':
        if active is not None and (root/'finish-deferred-plan').exists():
            (root/'finish-deferred-plan').unlink()
            complete()
        reply(request, {'rateLimits':{'planType':'pro','primary':{'usedPercent':25,'windowDurationMins':300,'resetsAt':1800000000}}})
    elif method == 'mcpServerStatus/list':
        assert params['threadId'] == thread_id
        if params.get('cursor') == 'page-two':
            reply(request, {'data':[{'name':'offline','runtimeStatus':'failed','authStatus':'unsupported','toolsError':'Connection refused','tools':{}}], 'nextCursor':None})
        else:
            reply(request, {'data':[{'name':'fixture','runtimeStatus':'connected' if mcp_enabled else 'disabled','authStatus':'notLoggedIn','httpOrigin':'https://fixture.example.com','tools':{'hello':{'name':'hello','inputSchema':{'type':'object'}}} if mcp_enabled else {}}], 'nextCursor':'page-two'})
    elif method == 'config/value/write':
        assert params['keyPath'] == 'mcp_servers."fixture".enabled'
        mcp_enabled = params['value']
        reply(request, {'status':'ok','overriddenMetadata':None})
    elif method == 'config/mcpServer/reload': reply(request,{})
    elif method == 'mcpServer/oauth/login': reply(request,{'authorizationUrl':'https://auth.example.com/login'})
    elif method == 'collaborationMode/list':
        if (root/'no-collaboration-modes').exists(): emit({'id':request['id'],'error':{'message':'unsupported fixture method collaborationMode/list'}})
        else: reply(request, {'data':[{'name':'Default','mode':'default'},{'name':'Plan','mode':'plan'}]})
    elif method == 'configRequirements/read': reply(request, {'requirements':{'allowedSandboxModes':['workspace-write','read-only'],'allowedApprovalsReviewers':['user']} if (root/'restrict-permissions').exists() else None})
    elif method == 'config/read': reply(request, {'config':{'model':'fixture-model','model_reasoning_effort':'high','developer_instructions':'Preserve inherited guidance.'},'origins':{}})
    elif method == 'thread/backgroundTerminals/list':
        reply(request, {'data':[{'command':'fixture shell','cwd':os.getcwd(),'itemId':'shell-item','processId':'123'}], 'nextCursor':None})
    elif method == 'skills/list':
        reply(request, {'data':[{'cwd':params['cwds'][0], 'skills':[{'name':'fixture-skill','description':'Fixture skill for workspace discovery','enabled':True,'path':str(pathlib.Path(params['cwds'][0])/'.agents/skills/fixture/SKILL.md')}], 'errors':[]}]})
    elif method == 'thread/compact/start':
        active = 'compact-'+str(turn)
        reply(request,{})
        event('turn/started', {'threadId':thread_id,'turn':{'id':active}})
        event('item/started', {'threadId':thread_id,'turnId':active,'item':{'id':'compaction','type':'contextCompaction'}})
        event('item/completed', {'threadId':thread_id,'turnId':active,'item':{'id':'compaction','type':'contextCompaction'}})
        event('turn/completed', {'threadId':thread_id,'turn':{'id':active,'status':'completed'}})
        active = None
    elif method == 'model/list': reply(request, {'data':[{'model':'fixture-model','isDefault':True,'defaultReasoningEffort':'high'}], 'nextCursor':None})
    elif method == 'thread/settings/update':
        if (root/'reject-mode-update').exists():
            emit({'id':request['id'],'error':{'code':-32601,'message':'Method not found: thread/settings/update'}})
        else:
            if 'sandboxPolicy' in params:
                sandbox = {'readOnly':'read-only','workspaceWrite':'workspace-write','dangerFullAccess':'danger-full-access'}[params['sandboxPolicy']['type']]
                workspace_policy={key:value for key,value in params['sandboxPolicy'].items() if key!='type'}
            approval_policy=params.get('approvalPolicy',approval_policy)
            approvals_reviewer=params.get('approvalsReviewer',approvals_reviewer)
            if 'collaborationMode' in params:
                collaboration_mode=params['collaborationMode']
                current_model=collaboration_mode['settings']['model']
                current_effort=collaboration_mode['settings'].get('reasoning_effort')
                (root/'codex-collaboration.json').write_text(json.dumps(collaboration_mode))
            event('thread/settings/updated',{'threadId':thread_id,'threadSettings':{'collaborationMode':collaboration_mode,'model':current_model,'effort':current_effort}})
            reply(request,{})
    elif method == 'thread/unsubscribe':
        if (root/'no-unsubscribe').exists():
            emit({'id':request['id'],'error':{'code':-32601,'message':'Method not found: thread/unsubscribe'}})
        else:
            status = 'unsubscribed' if loaded else 'notLoaded'
            loaded = False
            event('thread/closed',{'threadId':thread_id})
            event('thread/status/changed',{'threadId':thread_id,'status':{'type':'notLoaded'}})
            reply(request,{'status':status})
    elif method in ('thread/start','thread/resume'):
        if method == 'thread/start' and (root/'reject-plan-start').exists():
            emit({'id':request['id'],'error':{'code':-32603,'message':'Fixture context start failed'}})
            continue
        if (root/'distinct-threads').exists():
            thread_id = 'fixture-'+uuid.uuid4().hex if method == 'thread/start' else params['threadId']
        if method == 'thread/start':
            assert any(tool['name'] == 'difu_remove_questions' for tool in params['dynamicTools'])
        if method == 'thread/resume' and params.get('threadId','').startswith('missing-'):
            emit({'id':request['id'],'error':{'code':-32600,'message':f"no rollout found for thread {params['threadId']}"}})
            continue
        if method == 'thread/resume' and (root/'no-rollout').exists():
            emit({'id':request['id'],'error':{'message':'no rollout found'}})
            continue
        if method == 'thread/resume' and not loaded and (root/'fail-resume-after-unsubscribe').exists():
            (root/'fail-resume-after-unsubscribe').unlink()
            emit({'id':request['id'],'error':{'code':-32603,'message':'Transient fixture resume failure'}})
            continue
        # Native app-server rejoins an already loaded thread and ignores overrides.
        if method == 'thread/start': collaboration_mode=dict(mode='default',settings=dict(model=current_model,reasoning_effort=current_effort,developer_instructions=None))
        elif not loaded and (root/'codex-collaboration.json').exists(): collaboration_mode=json.loads((root/'codex-collaboration.json').read_text())
        if method == 'thread/start' or not loaded:
            sandbox = params.get('sandbox', sandbox)
            approval_policy = params.get('approvalPolicy', approval_policy)
            approvals_reviewer = params.get('approvalsReviewer', approvals_reviewer)
            current_model = params.get('model',current_model)
            current_effort = params.get('config',{}).get('model_reasoning_effort',current_effort)
            override = params.get('config',{}).get('sandbox_workspace_write',{})
            for source,target in [('network_access','networkAccess'),('writable_roots','writableRoots'),('exclude_tmpdir_env_var','excludeTmpdirEnvVar'),('exclude_slash_tmp','excludeSlashTmp')]:
                if source in override: workspace_policy[target] = override[source]
        loaded = True
        effective_sandbox = dict(type={'read-only':'readOnly','workspace-write':'workspaceWrite','danger-full-access':'dangerFullAccess'}[sandbox],**(workspace_policy if sandbox == 'workspace-write' else {}))
        reply(request, {'thread':{'id':thread_id},'model':current_model,'reasoningEffort':current_effort,'sandbox':effective_sandbox,'approvalPolicy':approval_policy,'approvalsReviewer':approvals_reviewer,'collaborationMode':collaboration_mode})
    elif method == 'turn/start':
        if (root/'drop-first-turn-before-ack').exists():
            (root/'drop-first-turn-before-ack').unlink()
            (root/'accepted-first-turn.json').write_text(json.dumps(dict(threadId=thread_id,input=params['input'])))
            sys.exit(0) # Accepted by the provider, but no acknowledgement or events reach Difu.
        if not loaded:
            emit({'id':request['id'],'error':{'code':-32000,'message':'Thread not loaded'}})
            continue
        current_model=params.get('model',current_model)
        current_effort=params.get('effort',current_effort)
        if 'sandboxPolicy' in params:
            workspace_policy={key:value for key,value in params['sandboxPolicy'].items() if key!='type'}
        if (root/'no-rollout').exists(): (root/'no-rollout').unlink()
        if 'sandboxPolicy' in params: sandbox = {'readOnly':'read-only','workspaceWrite':'workspace-write','dangerFullAccess':'danger-full-access'}[params['sandboxPolicy']['type']]
        approval_policy=params.get('approvalPolicy',approval_policy)
        # Native turn/start currently retains its loaded approvals reviewer;
        # exact preset changes must happen on thread/start or unloaded resume.
        collaboration_mode=params.get('collaborationMode',collaboration_mode)
        turn += 1
        active = 'turn-'+connection+'-'+str(turn)
        text = params['input'][0]['text']
        event('item/completed', {'threadId':thread_id,'turnId':active,'item':{'id':'user-'+connection+'-'+str(turn),'type':'userMessage','content':params['input']}})
        reply(request, {'turn':{'id':active,'status':'inProgress'}})
        event('turn/started', {'threadId':thread_id,'turn':{'id':active}})
        event('item/started', {'threadId':thread_id,'turnId':active,'item':{'id':'message-'+connection+'-'+str(turn),'type':'agentMessage','text':''}})
        event('item/agentMessage/delta', {'threadId':thread_id,'turnId':active,'itemId':'message-'+connection+'-'+str(turn),'delta':'Streaming fixture task'})
        if text.startswith('need edit') and sandbox == 'read-only':
            if 'with questions' in text:
                event('item/completed', {'threadId':thread_id,'turnId':active,'item':{
                    'id':'workspace-question','type':'agentMessage','delivery':'async','text':'One question',
                    'questions':[{'title':'Which detail?','options':['Brief','Full']}]}})
            emit({'id':900+turn,'method':'item/tool/call','params':{'threadId':thread_id,'turnId':active,'callId':'workspace','tool':'difu_begin_editing','arguments':{}}})
        elif text.startswith('register worktree: '):
            emit({'id':900+turn,'method':'item/tool/call','params':{'threadId':thread_id,'turnId':active,'tool':'difu_register_worktree','arguments':{'path':text.split(': ',1)[1]}}})
        elif text == 'delete this session':
            pathlib.Path('discard.txt').write_text('dirty self-deletion fixture')
            emit({'id':900+turn,'method':'item/tool/call','params':{'threadId':thread_id,'turnId':active,'tool':'difu_delete_session','arguments':{}}})
        elif text.startswith('artifact'):
            pathlib.Path('report.html').write_text('<h1>Fixture artifact</h1>')
            emit({'id':900+turn,'method':'item/tool/call','params':{'threadId':thread_id,'turnId':active,'tool':'difu_present_artifact','arguments':{'path':'report.html','title':'Fixture report'}}})
        elif text.startswith('fixture shell: '):
            subprocess.Popen([sys.executable, text.split(': ', 1)[1]], start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            complete()
        elif text.startswith('fixture final plan') or text == 'fixture checklist only':
            if text == 'fixture final plan with question':
                event('item/completed', {'threadId':thread_id,'turnId':active,'item':{'id':'plan-question-'+active,'type':'agentMessage','delivery':'async','text':'Optional clarification','questions':[{'title':'Any additional detail?'}]}})
            event('turn/plan/updated', {'threadId':thread_id,'turnId':active,'plan':[{'step':'Investigate','status':'completed'}]})
            if text != 'fixture checklist only':
                event('item/completed', {'threadId':thread_id,'turnId':active,'item':{'id':active+'-plan','type':'plan','text':'# Exact approved plan\n\nChange marker.txt to implemented; preserve unrelated changes.'}})
            if text == 'fixture final plan failed':
                event('turn/completed', {'threadId':thread_id,'turn':{'id':active,'status':'failed','error':{'message':'Fixture plan failed'}}})
                active = None
            elif text != 'fixture final plan deferred': complete()
        elif text.startswith('Implement the following approved plan.'):
            assert 'Change marker.txt to implemented; preserve unrelated changes.' in text
            assert collaboration_mode['mode'] == 'default'
            pathlib.Path('marker.txt').write_text('implemented\n')
            complete()
        elif text.startswith('chat only'):
            complete()
        elif text == 'fixture fail turn':
            event('turn/completed', {'threadId':thread_id,'turn':{'id':active,'status':'failed','error':{'message':'Fixture failure'}}})
            active = None
        elif text.startswith('approval'):
            emit({'id':900+turn,'method':'item/commandExecution/requestApproval','params':{'threadId':thread_id,'turnId':active,'itemId':'command','command':'git diff --check','reason':'Inspect diff only'}})
        elif text.startswith('app approval'):
            if sandbox == 'read-only':
                assert params['approvalPolicy']['granular']['mcp_elicitations']
                assert not params['approvalPolicy']['granular']['sandbox_approval']
            emit({'id':900+turn,'method':'mcpServer/elicitation/request','params':{'threadId':thread_id,'turnId':active,'serverName':'fixture-app','message':'Approve the explicitly requested app action','requestedSchema':{'type':'object','properties':{}}}})
        elif text.startswith('wait'):
            event('item/started', {'threadId':thread_id,'turnId':active,'item':{'id':'waiting-'+active,'type':'commandExecution','command':'fixture long-running tool','status':'inProgress'}})
        elif text == 'remove stale questions':
            question_id = 'stale-'+connection+'-'+str(turn)
            event('item/completed', {'threadId':thread_id,'turnId':active,'item':{
                'id':question_id,'type':'agentMessage','delivery':'async','text':'Two questions',
                'questions':[{'title':'Keep this question?'},{'title':'Stale question?'}]}})
            emit({'id':900+turn,'method':'item/tool/call','params':{'threadId':thread_id,'turnId':active,
                'tool':'difu_remove_questions','arguments':json.dumps({'questions':[
                    {'request_id':'difu-async:'+question_id,'question_id':'1'}]})}})
        elif text.startswith('async questions'):
            event('item/completed', {'threadId':thread_id,'turnId':active,'item':{
                'id':'questions-'+connection+'-'+str(turn),'type':'agentMessage','delivery':'async','text':'Three questions',
                'questions':[{'title':'Task?','options':['Explore','Review']},{'title':'Detail?','options':['Brief','Full']},{'title':'Anything else?','options':None}]}})
            if not text.endswith('active'):
                complete()
        elif text.startswith('Answers to your questions:') or text.startswith('> '):
            complete()
        elif text.startswith('question'):
            emit({'id':900+turn,'method':'item/tool/requestUserInput','params':{'threadId':thread_id,'turnId':active,'itemId':'question','isBlocking':True,'questions':[{'id':'color','question':'Which color?','header':'Color','options':[{'label':'Green','description':'Matrix'}]}]}})
        else:
            if sandbox == 'read-only':
                raise RuntimeError('fixture attempted a write before worktree creation')
            pathlib.Path('new.txt').write_text('agent change\n')
            complete()
    elif method == 'turn/steer':
        if params['expectedTurnId'] != active: emit({'id':request['id'],'error':{'message':'stale turn'}})
        else: reply(request, {'turnId':active})
    elif method == 'turn/interrupt':
        reply(request,{})
        event('turn/completed', {'threadId':thread_id,'turn':{'id':active,'status':'interrupted'}})
        active = None
    elif not method and 'result' in request:
        event('serverRequest/resolved', {'threadId':thread_id,'requestId':request['id']})
        if request['result'].get('decision') in ('accept','acceptForSession') or 'answers' in request['result']:
            pathlib.Path('approved.txt').write_text('approved change\n')
        if any(item.get('text') == 'Artifact registered in difu' for item in request['result'].get('contentItems', [])):
            complete()
        elif any('remaining_questions' in item.get('text', '') for item in request['result'].get('contentItems', [])):
            assert request['result']['success']
            remaining = json.loads(request['result']['contentItems'][0]['text'])['remaining_questions']
            assert len(remaining) == 1 and remaining[0]['question_id'] == '0'
            complete()
        elif 'contentItems' not in request['result']:
            complete()
    else: emit({'id':request.get('id'),'error':{'message':'unsupported fixture method '+str(method)}})
