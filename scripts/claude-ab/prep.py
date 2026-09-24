#!/usr/bin/env python3
"""Copy one Claude transcript under a fresh session id for an A/B arm; print the id.
Also writes the ground-truth edited-file set (never printed) next to the results."""
import json, os, re, sys, uuid, glob
src_prefix, arm = sys.argv[1], sys.argv[2]
cwd = os.environ.get('AB_CWD', os.path.expanduser('~'))
config = os.environ.get('CLAUDE_CONFIG_DIR', os.path.expanduser('~/.claude'))
proj = os.path.join(config, 'projects', re.sub(r'[^A-Za-z0-9]', '-', cwd))
src = [f for f in glob.glob(f'{proj}/{src_prefix}*.jsonl')][0]
new = str(uuid.uuid4())
old = os.path.basename(src)[:-6]
dst = f'{proj}/{new}.jsonl'
files = []
with open(src, encoding='utf-8', errors='replace') as i, open(dst, 'x', encoding='utf-8') as o:
    for line in i:
        try: r = json.loads(line)
        except Exception: continue
        if r.get('sessionId') == old: r['sessionId'] = new
        m = r.get('message') or {}
        if isinstance(m, dict) and isinstance(m.get('content'), list):
            for b in m['content']:
                if isinstance(b, dict) and b.get('type') == 'tool_use' and b.get('name') in ('Edit', 'Write', 'MultiEdit'):
                    p = (b.get('input') or {}).get('file_path')
                    if p and p not in files: files.append(p)
        o.write(json.dumps(r) + '\n')
os.chmod(dst, 0o600)
res = os.environ['AB_DIR']
json.dump({'copy': dst, 'source': src_prefix, 'arm': arm, 'truth': files}, open(f'{res}/{arm}-{src_prefix}-{new[:8]}.json', 'w'))
print(new)
