import json

with open('target/coverage/tarpaulin-report.html') as f:
    html = f.read()

idx = html.index('var data = ') + len('var data = ')
depth = 0
in_string = False
escape = False
end_idx = idx
for i in range(idx, len(html)):
    c = html[i]
    if escape:
        escape = False
        continue
    if c == '\\' and in_string:
        escape = True
        continue
    if c == '"' and not escape:
        in_string = not in_string
        continue
    if in_string:
        continue
    if c in '{[':
        depth += 1
    elif c in '}]':
        depth -= 1
        if depth == 0:
            end_idx = i + 1
            break

data = json.loads(html[idx:end_idx])

targets = ['btree.rs', 'pager.rs', 'cursor.rs', 'http_api.rs', 'mysql.rs', 'kk_backend.rs',
           'statement.rs', 'expr.rs', 'query.rs', 'prefix_compress.rs',
           'eval_expr.rs', 'exec_select.rs', 'exec_dml.rs', 'exec_ddl.rs', 'execute.rs',
           'schema.rs', 'types.rs', 'error.rs']

for f in data['files']:
    fname = f['path'][-1]
    if fname not in targets:
        continue
    full_path = '/'.join(f['path'])
    if not any(d in full_path for d in ['storage/', 'server/', 'sql/', 'vm/', 'src/']):
        continue
    traces = f.get('traces', [])
    uncovered_lines = sorted([t['line'] for t in traces if t.get('stats', {}).get('Line', 0) == 0])
    if not uncovered_lines:
        continue
    
    path = full_path.split('kkdb/')[-1]
    total = len(traces)
    covered = total - len(uncovered_lines)
    pct = covered/total*100 if total > 0 else 0
    print(f"\n{'='*60}")
    print(f"{path}: {len(uncovered_lines)} uncovered / {total} total ({pct:.1f}%)")
    print(f"{'='*60}")
    
    ranges = []
    start = uncovered_lines[0]
    prev = start
    for line in uncovered_lines[1:]:
        if line <= prev + 2:
            prev = line
        else:
            ranges.append((start, prev))
            start = line
            prev = line
    ranges.append((start, prev))
    ranges.sort(key=lambda r: r[1]-r[0], reverse=True)
    for s, e in ranges[:12]:
        print(f"  Lines {s}-{e} ({e-s+1} lines)")
