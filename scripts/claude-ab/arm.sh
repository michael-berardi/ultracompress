#!/bin/bash
# usage: AB_DIR=/private/dir arm.sh <transcript-id-prefix> <uc|stock>
# Resumes a COPY of one Claude Code transcript (fresh session id), runs /compact,
# records Claude's own compactMetadata, then asks one question and scores the
# answer against the transcript's Edit/Write file paths (counts only).
# stock = --settings disableAllHooks (Claude's built-in compaction);
# uc = the UltraCompress plugin's session.compact hook. Delete the copies after.
set -u
SRC=$1; ARM=$2; AB=${AB_DIR:?set AB_DIR to a private results directory}; T=${TMUX_BIN:-tmux}; HERE=$(cd "$(dirname "$0")" && pwd); MODEL=${AB_MODEL:-claude-opus-5-5}
ID=$(python3 $HERE/prep.py "$SRC" "${ARM_LABEL:-$ARM}"); SOCK=ut235ab
EXTRA=""; [ "$ARM" = stock ] && EXTRA="--settings '{\"disableAllHooks\":true}'"
cd "${AB_CWD:-$HOME}" && env -u TMUX -u TMUX_PANE -u CLAUDE_CODE_SESSION_ID -u CLAUDECODE $T -L $SOCK -f /dev/null new-session -d -s ab -x 160 -y 50 -c "${AB_CWD:-$HOME}" "claude --resume $ID --model $MODEL $EXTRA --debug-file $AB/$ARM-$SRC.debug"
for i in $(seq 1 60); do sleep 1; $T -L $SOCK capture-pane -p -t ab | grep -q "❯" && break; done
sleep 8
$T -L $SOCK send-keys -t ab -l "/compact"; sleep 0.3
S=$(python3 -c "import time;print(time.time())"); $T -L $SOCK send-keys -t ab Enter
for i in $(seq 1 1200); do sleep 0.5; s=$($T -L $SOCK capture-pane -p -t ab); echo "$s" | grep -q "Compacted\|rror" && break; done
E=$(python3 -c "import time;print(time.time())")
echo "{\"arm\":\"$ARM\",\"source\":\"$SRC\",\"id\":\"$ID\",\"wall_s\":$(python3 -c "print(round($E-$S,2))")}" >> $AB/walls.jsonl
Q="${ARM_Q:-Answer from memory only; do not call any tools. List every file path you created or edited with Edit or Write earlier in this session, one full path per line, and nothing else.}"
sleep 2; $T -L $SOCK send-keys -t ab -l "$Q"; sleep 0.3; $T -L $SOCK send-keys -t ab Enter
F=$(ls $AB/${ARM_LABEL:-$ARM}-$SRC-${ID:0:8}.json)
for i in $(seq 1 150); do sleep 2; python3 $HERE/score.py "$F" | grep -q '"final": "end_turn"' && break; done
$T -L $SOCK kill-server
python3 $HERE/score.py "$F"
python3 - "$F" <<'PY'
import json,sys
m=json.load(open(sys.argv[1]))
for line in open(m['copy'],encoding='utf-8',errors='replace'):
    try: r=json.loads(line)
    except: continue
    if r.get('subtype')=='compact_boundary':
        c=r['compactMetadata']; print(json.dumps({k:c.get(k) for k in ('trigger','preTokens','postTokens','durationMs')}))
PY
