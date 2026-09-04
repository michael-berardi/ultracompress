#!/usr/bin/env bash
# Live compaction benchmark: identical task, three compaction stacks.
#   stock-pi : Pi core LLM-summary compaction
#   ultracompress : Pi + ultracompress extension (VCC brief + snap frames + UC)
#   omp      : Oh My Pi forced to snapcompact-only (toolResults on, auto shape)
#
# Metrics per run: billed input tokens (incl. cache reads), output tokens,
# cost, wall time, compaction events, answer correctness.
set -euo pipefail

BENCH=/tmp/ultracompress-live-bench
WS=$BENCH/ws
OUT=$BENCH/results
MODEL_PI="zai/glm-5.3-flash"
THRESHOLD=23616
STORE=~/.pi/agent/models-store.json
ULTRACOMPRESS_EXT="$HOME/dev/ultracompress/extension/index.ts"
mkdir -p "$OUT"

gen_workspace() {
  rm -rf "$WS"
  mkdir -p "$WS/src" "$WS/logs" "$WS/data"
  # 8 source files, 12 functions each, countable by pattern "function <name>"
  local total=0
  for i in 1 2 3 4 5 6 7 8; do
    {
      for j in $(seq 1 12); do
        echo "function module${i}_handler${j}(input) {"
        echo "  const stage = ${i} * 100 + ${j};"
        echo "  return input + stage;"
        echo "}"
        echo ""
      done
    } > "$WS/src/module${i}.js"
    total=$((total + 12))
  done
  echo "$total" > "$BENCH/truth_functions.txt"

  # Two big logs (~50k chars each) with distinctive error codes.
  python3 - <<'PYEOF'
import random
random.seed(42)
def log(path, lines, err_code, err_line):
    with open(path, "w") as f:
        for i in range(lines):
            if i == lines // 2:
                f.write(err_line.format(code=err_code, n=i) + "\n")
            else:
                f.write(f"[{i:05d}] INFO worker pool tick processed=ok latency={random.randint(2,40)}ms queue={random.randint(0,12)}\n")
log("/tmp/ultracompress-live-bench/ws/logs/deploy.log", 700, "E-8341-DEPLOY", "[{n:05d}] FATAL deploy orchestrator rollback code={code} stage=canary")
log("/tmp/ultracompress-live-bench/ws/logs/build.log", 700, "W-2210-CACHE", "[{n:05d}] WARN build cache evicted entry=stale size={n}kb")
# Two JSON data files (~12k chars each)
import json
for name, n in (("inventory", 120), ("telemetry", 90)):
    data = {"records": [{"id": k, "name": f"{name}-{k:04d}", "tags": ["a","b","c"], "score": k*3} for k in range(n)]}
    open(f"/tmp/ultracompress-live-bench/ws/data/{name}.json", "w").write(json.dumps(data, indent=1))
PYEOF
}

patch_window() {
  python3 - "$STORE" <<'PYEOF'
import json, shutil, sys
p = sys.argv[1]
shutil.copy(p, p + ".ultracompress-bench-backup")
m = json.load(open(p))
for x in m["zai"]["models"]:
    if x["id"] == "glm-5.3-flash":
        x["contextWindow"] = 40000
json.dump(m, open(p, "w"))
print("contextWindow -> 40000 (threshold ~23.6k)")
PYEOF
}

restore_window() {
  local bak="$STORE.ultracompress-bench-backup"
  [ -f "$bak" ] && cp "$bak" "$STORE" && rm -f "$bak" && echo "models-store restored"
}
trap 'restore_window 2>/dev/null || true' EXIT

run_stock_pi() {
  echo "── stock-pi ──"
  local dir=$WS-stock
  rm -rf "$dir"; cp -r "$WS" "$dir"
  patch_window
  local t0=$(date +%s)
  (cd "$dir" && timeout 900 pi --print --no-extensions --no-skills --no-prompt-templates \
      --model "$MODEL_PI" "$(cat $BENCH/task.txt)" > "$OUT/stock.jsonl" 2> "$OUT/stock.err") || true
  restore_window
  echo "$(( $(date +%s) - t0 ))" > "$OUT/stock.secs"
}

run_ultracompress() {
  echo "── ultracompress ──"
  local dir=$WS-ultracompress
  rm -rf "$dir"; cp -r "$WS" "$dir"
  patch_window
  local t0=$(date +%s)
  (cd "$dir" && timeout 900 pi --print --no-extensions --no-skills --no-prompt-templates \
      -e "$ULTRACOMPRESS_EXT" \
      --model "$MODEL_PI" "$(cat $BENCH/task.txt)" > "$OUT/ultracompress.jsonl" 2> "$OUT/ultracompress.err") || true
  restore_window
  echo "$(( $(date +%s) - t0 ))" > "$OUT/ultracompress.secs"
}

run_omp() {
  echo "── omp snapcompact ──"
  local dir=$WS-omp
  rm -rf "$dir"; cp -r "$WS" "$dir"
  # Force OMP to its best snapcompact-only configuration.
  omp config set compaction.methodOrder '["snapcompact"]' >/dev/null 2>&1 || true
  omp config set snapcompact.toolResults true >/dev/null 2>&1 || true
  omp config set compaction.thresholdTokens "$THRESHOLD" >/dev/null 2>&1 || true
  local t0=$(date +%s)
  (cd "$dir" && timeout 900 omp --model zai/glm-5.3-flash --mode json --no-session \
      --approval-mode yolo --print "$(cat $BENCH/task.txt)" > "$OUT/omp.jsonl" 2> "$OUT/omp.err") || true
  echo "$(( $(date +%s) - t0 ))" > "$OUT/omp.secs"
  # Restore defaults.
  omp config set compaction.methodOrder '["remote","snapcompact","handoff","shake","soft"]' >/dev/null 2>&1 || true
  omp config set snapcompact.toolResults false >/dev/null 2>&1 || true
  omp config set compaction.thresholdTokens -1 >/dev/null 2>&1 || true
}

case "${1:-all}" in
  gen)   gen_workspace ;;
  stock) run_stock_pi ;;
  ultracompress|rc) run_ultracompress ;;
  omp)   run_omp ;;
  patch) patch_window ;;
  restore) restore_window ;;
  all)   gen_workspace; run_stock_pi; run_ultracompress; run_omp ;;
esac
