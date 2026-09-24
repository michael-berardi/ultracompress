#!/usr/bin/env python3
"""Score the last assistant answer in an arm's transcript copy against the
ground-truth edited-file set. Prints counts only, never paths or content."""
import json, sys, os
meta = json.load(open(sys.argv[1]))
truth = meta['truth']
last = None; used_tools = False; after_q = False; final = None
for line in open(meta['copy'], encoding='utf-8', errors='replace'):
    try: r = json.loads(line)
    except Exception: continue
    m = r.get('message') or {}
    if r.get('type') == 'user' and isinstance(m.get('content'), str) and ('Answer from memory only' in m['content'] or 'Use the ultracompress_recall tool' in m['content']):
        after_q = True; last = None; used_tools = False; continue
    if r.get('type') == 'user' and isinstance(m.get('content'), list):
        for b in m['content']:
            if isinstance(b, dict) and b.get('type') == 'text' and ('Answer from memory only' in b.get('text', '') or 'Use the ultracompress_recall tool' in b.get('text', '')):
                after_q = True; last = None; used_tools = False
    if after_q and r.get('type') == 'assistant' and isinstance(m.get('content'), list):
        for b in m['content']:
            if isinstance(b, dict) and b.get('type') == 'tool_use': used_tools = True
            if isinstance(b, dict) and b.get('type') == 'text' and b.get('text', '').strip(): last = b['text']
        final = m.get('stop_reason')
answer = last or ''
full = sum(1 for p in truth if p in answer)
base = sum(1 for p in truth if os.path.basename(p) in answer)
lines = [l for l in answer.splitlines() if l.strip()]
print(json.dumps({'arm': meta['arm'], 'source': meta['source'], 'truth': len(truth), 'full_path_hits': full,
                  'basename_hits': base, 'answer_lines': len(lines), 'used_tools': used_tools, 'answered': bool(answer), 'final': final}))
