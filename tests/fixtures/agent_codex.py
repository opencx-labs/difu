#!/usr/bin/env python3
"""Deterministic app-server fixture: never runs a model or project validation."""
import json, os, pathlib, sys, uuid
root = pathlib.Path(os.environ['DIFU_AGENT_FIXTURE'])
if sys.argv[1:3] == ['mcp', 'list']:
    print('[]')
    sys.exit(0)
if sys.argv[1:2] == ['exec']:
    prompt = sys.stdin.read()
    with (root/'titles.jsonl').open('a') as log:
        log.write(json.dumps({'args':sys.argv[1:], 'input':json.loads(prompt)})+'\n')
    output = pathlib.Path(sys.argv[sys.argv.index('--output-last-message')+1])
    output.write_text(json.dumps({'title':'Fixture coding session'}))
    sys.exit(0)
turn = 0
connection = uuid.uuid4().hex[:8]
sandbox = 'workspace-write'
active = None
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
    elif method == 'config/read': reply(request, {'config':{'model':'fixture-model','model_reasoning_effort':'high','developer_instructions':'Preserve inherited guidance.'},'origins':{}})
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
    elif method in ('thread/start','thread/resume'):
        sandbox = params.get('sandbox', sandbox)
        reply(request, {'thread':{'id':thread_id},'model':params.get('model','fixture-model'),'reasoningEffort':params.get('config',{}).get('model_reasoning_effort','high'),'sandbox':{'type':'readOnly' if sandbox == 'read-only' else 'workspaceWrite'},'approvalPolicy':params.get('approvalPolicy','on-request'),'approvalsReviewer':'user'})
    elif method == 'turn/start':
        if params.get('sandboxPolicy',{}).get('type') == 'readOnly': sandbox = 'read-only'
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
        elif text.startswith('chat only'):
            complete()
        elif text.startswith('approval'):
            emit({'id':900+turn,'method':'item/commandExecution/requestApproval','params':{'threadId':thread_id,'turnId':active,'itemId':'command','command':'git diff --check','reason':'Inspect diff only'}})
        elif text.startswith('wait'):
            pass
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
        if 'contentItems' not in request['result']:
            complete()
    else: emit({'id':request.get('id'),'error':{'message':'unsupported fixture method '+str(method)}})
