#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ['DIFU_TEST_FIXTURE'])
revs = json.loads((root / 'revisions.json').read_text())
args = sys.argv[1:]
url = 'https://github.com/example/project/pull/1'
if args[0] == 'search':
    value = [dict(number=1, title='Describe the behavior', url=url, author=dict(login='author'), updatedAt='2026-09-15T00:00:00Z', isDraft=False)]
elif args[0:2] == ['pr', 'checks']:
    print(json.dumps([dict(name='unit tests', state='IN_PROGRESS', bucket='pending', startedAt='2026-09-15T00:00:00Z', completedAt='', link=url+'/checks')]))
    sys.exit(8)
elif 'timeline?' in args[-1]:
    value = [[dict(event='commented', user=dict(login='reviewer'), body='A review comment', created_at='2026-09-15T00:01:00Z', html_url=url), dict(event='committed', author=dict(name='author'), committer=dict(date='2026-09-15T00:00:00Z'), sha=revs['head'], message='Change behavior', html_url=url)]]
elif '/comments?' in args[-1]:
    value = [[]]
else:
    value = dict(title='Describe the behavior', body='PR description with `code`.', user=dict(login='author'), head=dict(sha=revs['head'], ref='feature'), base=dict(sha=revs['base'], ref='main'), state='open', merged=False, additions=1, deletions=1, changed_files=1)
print(json.dumps(value))
