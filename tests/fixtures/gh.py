#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ['DIFU_TEST_FIXTURE'])
revs = json.loads((root / 'revisions.json').read_text())
args = sys.argv[1:]
url = 'https://github.com/example/project/pull/1'
if args[:2] == ['pr', 'view']:
    assert args == ['pr', 'view', '--json=url']
    if (root / 'no-branch-pr').exists():
        print('no pull requests found for branch', file=sys.stderr);sys.exit(1)
    print(json.dumps(dict(url=url)));sys.exit(0)
if args[:2] == ['pr', 'merge']:
    assert '--match-head-commit' in args
    assert args[args.index('--match-head-commit')+1] == revs['head']
    with (root / 'writes.jsonl').open('a') as log: log.write(json.dumps(dict(merge=args))+'\n')
    print('Merged fixture PR')
    sys.exit(0)
if args[:2] == ['api', '--method']:
    method, endpoint = args[2:4]
    body = json.load(sys.stdin) if '--input' in args else None
    if endpoint == 'graphql':
        query = body['query']
        if query.startswith('mutation'):
            if (root / 'fail-write').exists():
                print('Fixture write rejected',file=sys.stderr);sys.exit(1)
            with (root / 'writes.jsonl').open('a') as log: log.write(json.dumps(body)+'\n')
            if 'AddPullRequestReviewInput' in query and 'event' not in body['variables']['input']:
                (root / 'pending-review').write_text('pending-1')
            if 'SubmitPullRequestReviewInput' in query:
                (root / 'pending-review').unlink(missing_ok=True)
            if 'MarkFileAsViewedInput' in query: (root / 'viewed').write_text('yes')
            if 'UnmarkFileAsViewedInput' in query: (root / 'viewed').unlink(missing_ok=True)
            value = dict(data=dict(result=dict(clientMutationId=None)))
        elif 'participants' in query:
            value = dict(data=dict(repository=dict(pullRequest=dict(participants=dict(nodes=[dict(login='author'),dict(login='reviewer')],pageInfo=dict(hasNextPage=False,endCursor=None))))))
        else:
            assert 'reviews(first:1,states:[PENDING],author:$login)' in query
            pending = [dict(id='pending-1')] if (root/'pending-review').exists() else []
            value = dict(data=dict(repository=dict(pullRequest=dict(id='PR_fixture',headRefOid=revs['head'],reviews=dict(nodes=pending),files=dict(nodes=[dict(path='main.rs',viewerViewedState='VIEWED' if (root/'viewed').exists() else 'UNVIEWED')],pageInfo=dict(hasNextPage=False,endCursor=None))))))
    elif method != 'GET':
        with (root / 'writes.jsonl').open('a') as log: log.write(json.dumps(dict(endpoint=endpoint,method=method,body=body))+'\n')
        value = dict(id=100)
    elif '/collaborators?' in endpoint: value = [dict(login='alice'),dict(login='author'),dict(login='reviewer')]
    elif '/teams?' in endpoint: value = [dict(slug='platform')]
    elif endpoint == 'user': value = dict(login='reviewer')
    elif endpoint.startswith('users/'): value = dict(type='Organization')
    elif endpoint.startswith('orgs/'): value = [dict(login='alice'),dict(login='reviewer')]
    else: raise AssertionError(endpoint)
    print(json.dumps(value));sys.exit(0)
if args[0] == 'search':
    if (root / 'fail-search').exists():
        print('Fixture GitHub unavailable', file=sys.stderr)
        sys.exit(1)
    with (root / 'searches.jsonl').open('a') as log:
        log.write(json.dumps(args) + '\n')
    assert any(a.startswith(('--review-requested=', '--author=', '--repo=')) for a in args), 'Unscoped search'
    number, title = (2, 'My authored change') if '--author=@me' in args else (1, 'Describe the behavior')
    if '--repo=example/second' in args:
        number, title = 3, 'Second repository change'
        url = 'https://github.com/example/second/pull/3'
    else:
        url = f'https://github.com/example/project/pull/{number}'
    value = [dict(number=number, title=title, url=url, author=dict(login='author'), updatedAt='2026-09-15T00:00:00Z', createdAt='2026-09-10T12:30:00Z', isDraft=False)]
elif args[0:2] == ['api', 'graphql'] and any('statusCheckRollup' in a for a in args):
    mode = (root / 'status-case').read_text() if (root / 'status-case').exists() else 'normal'
    check = dict(__typename='CheckRun', name='unit tests', status='IN_PROGRESS', conclusion=None, startedAt='2026-09-15T00:00:00Z', completedAt='', detailsUrl=url+'/checks', checkSuite=dict(app=dict(databaseId=15368)))
    if mode in ('failure', 'rules-denied'):
        check.update(status='COMPLETED', conclusion='FAILURE', detailsUrl='https://github.com/example/project/actions/runs/1/job/2')
    contexts = dict(nodes=[check], pageInfo=dict(hasNextPage=False, endCursor=None))
    rollup = None if mode in ('conflict', 'unknown') else dict(contexts=contexts)
    pr = dict(mergeable='CONFLICTING' if mode=='conflict' else 'UNKNOWN' if mode=='unknown' else 'MERGEABLE', mergeStateStatus='DIRTY' if mode=='conflict' else 'CLEAN', headRefOid=revs['head'], baseRefOid=revs['base'], state='OPEN', baseRef=dict(name='main',branchProtectionRule=None), commits=dict(nodes=[dict(commit=dict(statusCheckRollup=rollup))]))
    value = dict(data=dict(repository=dict(pullRequest=pr)))
elif '/rules/branches/' in args[-1]:
    mode = (root / 'status-case').read_text() if (root / 'status-case').exists() else 'normal'
    if mode=='rules-denied':
        print('Fixture rules access denied',file=sys.stderr);sys.exit(1)
    value = [[dict(type='required_status_checks',parameters=dict(required_status_checks=[dict(context='lint'),dict(context='lint'),dict(context='typecheck')]))]] if mode=='conflict' else [[]]
elif args[-1]=='repos/example/project/actions/jobs/2/logs':
    print('2026-09-16T13:10:00Z  FAIL  mail.spec.ts > Mail > sends reply')
    print('2026-09-16T13:10:01Z AssertionError: expected reply')
    sys.exit(0)
elif args[0:2] == ['api', 'graphql'] and 'changedFiles' in args[-1]:
    import re
    aliases = re.findall(r'(r[0-9]+): repository', args[-1])
    assert 0 < len(aliases) <= 25
    with (root / 'stats-batches').open('a') as log:
        log.write(str(len(aliases)) + '\n')
    value = dict(data={alias: dict(pullRequest=dict(additions=1, deletions=1, changedFiles=1)) for alias in aliases})
elif args[0:2] == ['api', 'graphql']:
    with (root / 'revision-polls').open('a') as log:
        log.write('poll\n')
    assert 'headRefOid baseRefOid' in args[-1]
    value = dict(data=dict(repository=dict(pullRequest=dict(headRefOid=revs['head'], baseRefOid=revs['base']))))
elif 'user/repos?' in args[-1]:
    assert '--paginate' in args and '--slurp' in args
    value = [[dict(full_name='example/project')], [dict(full_name='example/second')]]
elif args[0:2] == ['pr', 'checks']:
    print(json.dumps([dict(name='unit tests', state='IN_PROGRESS', bucket='pending', startedAt='2026-09-15T00:00:00Z', completedAt='', link=url+'/checks')]))
    sys.exit(8)
elif 'timeline?' in args[-1]:
    value = [[dict(event='commented', user=dict(login='reviewer'), body='A review comment', created_at='2026-09-15T00:01:00Z', html_url=url), dict(event='committed', author=dict(name='author'), committer=dict(date='2026-09-15T00:00:00Z'), sha=revs['head'], message='Change behavior', html_url=url)]]
elif '/comments?' in args[-1]:
    value = [[]]
else:
    value = dict(title='Describe the behavior', body='PR description with `code`.', user=dict(login='author'), head=dict(sha=revs['head'], ref='feature'), base=dict(sha=revs['base'], ref='main'), state='open', merged=False, additions=1, deletions=1, changed_files=1)
if (root / 'updated-title').exists():
    if isinstance(value, list) and value and isinstance(value[0], dict) and 'title' in value[0]:
        value[0]['title'] = 'Fresh title from GitHub'
    elif isinstance(value, dict) and 'title' in value:
        value['title'] = 'Fresh title from GitHub'
print(json.dumps(value))
