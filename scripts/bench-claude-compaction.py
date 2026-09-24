#!/usr/bin/env python3
"""
bench-claude-compaction.py — offline benchmark: UltraCompress vs Claude Code stock compaction.

Phase 1 (no model calls, no network): scan Claude Code transcripts for
`compact_boundary` records (type=system, subtype=compact_boundary), rebuild the
conversation each stock compaction summarized, feed that same conversation to
`ultracompress compact --policy auto --vision auto` with the stdin defaults of
claude-code/hooks/ultracompress.mjs (toEntries mapping, keepUserTurns retry on
"no safe cut point"), and compare wall-clock time and token counts.

PRIVACY: transcripts are read programmatically in place (or from --transcripts).
Nothing but numbers, counts, model ids, Claude Code version strings and
anonymous sha256(path+uuid)[:12] sample ids is printed or written — never
message text, paths, commands or tool output.

Stdlib only. Usage:
  python3 bench-claude-compaction.py [--transcripts DIR] [--out FILE]
                                     [--limit N] [--bin PATH] [--timeout S]
Default transcripts root: ~/.claude/projects (scanned as DIR/*/*.jsonl plus
DIR/*.jsonl). Transcripts over --max-file-bytes are skipped. Per-sample
UltraCompress timeout: --timeout (default 60s).
"""

import argparse
import glob
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

DEFAULT_BIN = os.path.join(os.path.expanduser("~"), ".ultraterm", "bin", "ultracompress")
DEFAULT_TRANSCRIPTS = os.path.join(os.path.expanduser("~"), ".claude", "projects")
MAX_FILE_BYTES_DEFAULT = 256 * 1024 * 1024

# Exact stdin defaults of buildCompactStdin()/COMPACT_DEFAULTS in
# claude-code/hooks/ultracompress.mjs (policy/keepUserTurns are set per run).
COMPACT_DEFAULTS = {
    "smartKeepTail": True,
    "vision": "auto",
    "modelVision": None,
    "ucBin": "uc",
    "ucEnabled": True,
    "ucMinChars": 8192,
    "snapMinChars": 8192,
}

ENTRY_ID_RE = re.compile(r"^m(\d+)(?::|$)")
NO_CUT_RE = re.compile(r"no safe cut point")


# ---------------------------------------------------------------- transcripts

def iter_transcript_files(root, max_bytes):
    """Sorted *.jsonl under root/*/*.jsonl and root/*.jsonl; oversize skipped."""
    pats = [os.path.join(root, "*", "*.jsonl"), os.path.join(root, "*.jsonl")]
    seen, files, oversize = set(), [], 0
    for pat in pats:
        for p in sorted(glob.glob(pat)):
            if p in seen:
                continue
            seen.add(p)
            try:
                if os.path.getsize(p) > max_bytes:
                    oversize += 1
                    continue
            except OSError:
                continue
            files.append(p)
    return files, oversize


def load_records(path):
    """Parse a transcript; returns (records, unparseable_lines). Never logs content."""
    recs, bad = [], 0
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    recs.append(json.loads(line))
                except ValueError:
                    bad += 1
    except OSError:
        return None, 0
    return recs, bad


def is_boundary(rec):
    return (
        isinstance(rec, dict)
        and rec.get("type") == "system"
        and rec.get("subtype") == "compact_boundary"
    )


def reconstruct(recs, by_uuid, boundary):
    """Walk parentUuid back from logicalParentUuid; stop at a previous compact
    boundary (the task's lineage rule). Returns (chain, partial, chained, stop)."""
    chain, partial, chained, stop = [], False, False, "root"
    cur = boundary.get("logicalParentUuid")
    guard = 0
    while cur and guard < 1_000_000:
        guard += 1
        rec = by_uuid.get(cur)
        if rec is None:
            partial, stop = True, "missing-parent"
            break
        if is_boundary(rec):
            chained, stop = True, "boundary"
            break
        chain.append(rec)
        cur = rec.get("parentUuid")
        if not cur:
            stop = "root"
            break
    return chain, partial, chained, stop


def nearest_meta(recs, boundary_idx, field):
    """Model / version from the nearest non-sidechain assistant record above the
    boundary (fallback: any record); returns a string or None."""
    for sidechain in (False, True):
        for i in range(boundary_idx - 1, -1, -1):
            r = recs[i]
            if not isinstance(r, dict) or bool(r.get("isSidechain")) is sidechain:
                continue
            if field == "model":
                msg = r.get("message")
                if r.get("type") == "assistant" and isinstance(msg, dict):
                    m = msg.get("model")
                    if isinstance(m, str) and m:
                        return m
            else:
                v = r.get("version")
                if isinstance(v, str) and v:
                    return v
    return None


# ------------------------------------------------- toEntries (plugin parity)

def to_session_row(rec):
    """Raw Claude transcript record -> SessionMessage row (role/text/toolUses/
    toolResults), the shape ultracompress.mjs's toEntries() consumes."""
    rtype = rec.get("type")
    if rtype not in ("user", "assistant"):
        return None
    msg = rec.get("message") if isinstance(rec.get("message"), dict) else {}
    content = msg.get("content")
    texts, uses, results = [], [], []
    if isinstance(content, str):
        if content:
            texts.append(content)
    elif isinstance(content, list):
        for b in content:
            if not isinstance(b, dict):
                continue
            bt = b.get("type")
            if bt == "text" and isinstance(b.get("text"), str) and b["text"]:
                texts.append(b["text"])
            elif bt == "tool_use":
                uses.append(
                    {
                        "tool_use_id": b.get("id"),
                        "tool": b.get("name") if isinstance(b.get("name"), str) else "",
                        "input": b.get("input") if isinstance(b.get("input"), dict) else {},
                    }
                )
            elif bt == "tool_result":
                rc = b.get("content")
                rtext = []
                if isinstance(rc, str):
                    rtext.append(rc)
                elif isinstance(rc, list):
                    for rb in rc:
                        if isinstance(rb, dict) and rb.get("type") == "text" and isinstance(rb.get("text"), str):
                            rtext.append(rb["text"])
                results.append(
                    {
                        "tool_use_id": b.get("tool_use_id"),
                        "text": "\n".join(rtext),
                        "isError": bool(b.get("is_error")),
                    }
                )
    return {
        "role": "assistant" if (msg.get("role") or rtype) == "assistant" else "user",
        "text": "\n".join(texts),
        "toolUses": uses,
        "toolResults": results,
    }


def to_entries(rows):
    """Port of toEntries() in claude-code/hooks/ultracompress.mjs: text blocks,
    tool_use -> toolCall, tool_result -> toolResult with tool name; ids m<i>."""
    entries, names = [], {}
    for i, m in enumerate(rows):
        eid = f"m{i}"
        if not m or m.get("role") not in ("assistant", "user"):
            continue
        if m["role"] == "assistant":
            uses = m.get("toolUses") if isinstance(m.get("toolUses"), list) else []
            for u in uses:
                if u and u.get("tool_use_id"):
                    names[u["tool_use_id"]] = u.get("tool") if isinstance(u.get("tool"), str) else ""
            parts = []
            if isinstance(m.get("text"), str) and m["text"]:
                parts.append({"type": "text", "text": m["text"]})
            for u in uses:
                parts.append(
                    {
                        "type": "toolCall",
                        "id": u.get("tool_use_id"),
                        "name": u.get("tool") if isinstance(u.get("tool"), str) else "",
                        "arguments": u.get("input") if isinstance(u.get("input"), dict) else {},
                    }
                )
            if parts:
                entries.append({"type": "message", "id": eid, "message": {"role": "assistant", "content": parts}})
        else:
            parts = []
            if isinstance(m.get("text"), str) and m["text"]:
                parts.append({"type": "text", "text": m["text"]})
            if parts:
                entries.append({"type": "message", "id": eid, "message": {"role": "user", "content": parts}})
            results = m.get("toolResults") if isinstance(m.get("toolResults"), list) else []
            for j, r in enumerate(results):
                tid = r.get("tool_use_id") if r else None
                entries.append(
                    {
                        "type": "message",
                        "id": f"{eid}:r{j}",
                        "message": {
                            "role": "toolResult",
                            "toolCallId": tid,
                            "toolName": names.get(tid, "") if tid and tid in names else "",
                            "content": [
                                {"type": "text", "text": r.get("text") if r and isinstance(r.get("text"), str) else ""}
                            ],
                            "isError": bool(r and r.get("isError")),
                        },
                    }
                )
    return entries


def first_kept_index(entry_id):
    """Port of firstKeptIndexFromId(): `m12` / `m12:r0` -> 12; else -1."""
    m = ENTRY_ID_RE.match(entry_id) if isinstance(entry_id, str) else None
    return int(m.group(1)) if m else -1


def choose_cut(rows, kept_index):
    """Port of chooseCut(): first clean user turn (no toolResults) at/after
    kept_index; -1 when none — the plugin's retry condition."""
    i = max(0, min(kept_index if kept_index >= 0 else 0, len(rows)))
    while i < len(rows):
        m = rows[i]
        if m and m.get("role") == "user" and not (m.get("toolResults") or []):
            return i
        i += 1
    return -1


# ------------------------------------------------------------ ultra compress

def run_uc(bin_path, entries, keep_user_turns, timeout_s):
    """One `ultracompress compact` subprocess with plugin stdin defaults.
    Returns dict(ok, rc, stderr, ms, no_cut_stderr, timeout)."""
    payload = dict(COMPACT_DEFAULTS)
    payload.update({"policy": "auto", "keepUserTurns": keep_user_turns, "entries": entries})
    env = {k: v for k, v in os.environ.items() if k not in ("UC_TELEMETRY", "UC_TELEMETRY_PATH")}
    env["UC_TEXT_ENVELOPES"] = "1"  # same text envelope setting the plugin bridge uses
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            [bin_path, "compact", "--policy", "auto", "--vision", "auto"],
            input=json.dumps(payload).encode("utf-8"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            timeout=timeout_s,
        )
    except subprocess.TimeoutExpired:
        return {"ok": False, "timeout": True, "no_cut_stderr": False, "stderr": "", "rc": None, "ms": None}
    ms = (time.perf_counter() - t0) * 1000.0
    stderr = proc.stderr.decode("utf-8", "replace")
    ok = proc.returncode == 0
    rc = None
    if ok:
        try:
            rc = json.loads(proc.stdout.decode("utf-8", "replace"))
        except ValueError:
            ok = False
    return {"ok": ok, "rc": rc, "stderr": stderr, "ms": ms, "no_cut_stderr": bool(NO_CUT_RE.search(stderr)), "timeout": False}


def compact_like_plugin(bin_path, rows, entries, timeout_s):
    """Plugin posture: first attempt with keepUserTurns default (null); retry
    with keepUserTurns 0 on 'no safe cut point' or no clean cut. Returns
    (attempt, retried, reason)."""
    attempt = run_uc(bin_path, entries, None, timeout_s)
    retried = False
    no_cut = False
    if attempt["ok"]:
        kept = first_kept_index((attempt["rc"] or {}).get("first_kept_entry_id"))
        no_cut = choose_cut(rows, kept) < 0
    elif attempt["no_cut_stderr"]:
        no_cut = True
    if not attempt["ok"] or no_cut:
        retried = True
        attempt = run_uc(bin_path, entries, 0, timeout_s)
    if not attempt["ok"]:
        reason = "uc timeout" if attempt["timeout"] else "uc failed"
        return attempt, retried, reason
    rc = attempt["rc"] or {}
    if not str(rc.get("summary") or "").strip():
        return attempt, retried, "empty uc summary"
    return attempt, retried, None


# ------------------------------------------------------------------ aggregate

def pctl(values, q):
    """Linear-interpolation percentile; None when empty."""
    xs = sorted(v for v in values if isinstance(v, (int, float)) and math.isfinite(v))
    if not xs:
        return None
    pos = (len(xs) - 1) * q
    lo, hi = math.floor(pos), math.ceil(pos)
    if lo == hi:
        return xs[lo]
    return xs[lo] + (xs[hi] - xs[lo]) * (pos - lo)


def med(values):
    return pctl(values, 0.5)


def r1(x):
    return None if x is None else round(x, 1)


def r3(x):
    return None if x is None else round(x, 3)


def main():
    ap = argparse.ArgumentParser(description="Offline UltraCompress vs Claude Code stock compaction benchmark.")
    ap.add_argument("--transcripts", default=DEFAULT_TRANSCRIPTS, help="Transcripts root (DIR/*/*.jsonl).")
    ap.add_argument("--out", default=None, help="Write results JSON here.")
    ap.add_argument("--limit", type=int, default=60, help="Max samples to run through UltraCompress.")
    ap.add_argument("--bin", default=os.environ.get("ULTRACOMPRESS_BIN") or DEFAULT_BIN, help="ultracompress binary.")
    ap.add_argument("--timeout", type=float, default=60.0, help="Per-sample UltraCompress timeout (s).")
    ap.add_argument("--max-file-bytes", type=int, default=MAX_FILE_BYTES_DEFAULT, help="Skip transcripts over this size.")
    args = ap.parse_args()

    if not os.path.isfile(args.bin):
        print(f"error: ultracompress binary not found at {args.bin}", file=sys.stderr)
        return 2
    try:
        bin_ver = subprocess.run([args.bin, "--version"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10
                                 ).stdout.decode("utf-8", "replace").strip()
    except Exception:
        bin_ver = "unknown"

    files, oversize = iter_transcript_files(args.transcripts, args.max_file_bytes)
    skipped, samples = {}, []
    unparseable = 0
    stop_reached = False

    for path in files:
        if stop_reached:
            break
        recs, bad = load_records(path)
        if recs is None:
            skipped["unreadable file"] = skipped.get("unreadable file", 0) + 1
            continue
        unparseable += bad
        by_uuid = {}
        for r in recs:
            if isinstance(r, dict) and isinstance(r.get("uuid"), str):
                by_uuid.setdefault(r["uuid"], r)
        for idx, rec in enumerate(recs):
            if not is_boundary(rec) or stop_reached:
                continue
            cm = rec.get("compactMetadata") if isinstance(rec.get("compactMetadata"), dict) else {}
            sample_id = hashlib.sha256(((path or "") + str(rec.get("uuid") or "")).encode("utf-8")).hexdigest()[:12]
            chain, partial, chained, stop = reconstruct(recs, by_uuid, rec)
            if partial:
                skipped[f"partial lineage ({stop})"] = skipped.get(f"partial lineage ({stop})", 0) + 1
                continue
            rows = [r for r in (to_session_row(r) for r in chain) if r is not None]
            entries = to_entries(rows)
            if not entries:
                skipped["empty reconstruction"] = skipped.get("empty reconstruction", 0) + 1
                continue
            if len(samples) >= args.limit:
                stop_reached = True
                break
            attempt, retried, reason = compact_like_plugin(args.bin, rows, entries, args.timeout)
            if reason:
                skipped[reason] = skipped.get(reason, 0) + 1
                continue
            stats = attempt["rc"].get("stats") if isinstance(attempt["rc"].get("stats"), dict) else {}
            samples.append(
                {
                    "id": sample_id,
                    "preTokens": cm.get("preTokens"),
                    "postTokens": cm.get("postTokens"),
                    "stock_ms": cm.get("durationMs"),
                    "trigger": cm.get("trigger"),
                    "model": nearest_meta(recs, idx, "model"),
                    "version": rec.get("version") or nearest_meta(recs, idx, "version"),
                    "uc_ms": r1(attempt["ms"]),
                    "uc_tokens_before_est": stats.get("tokens_before_est"),
                    "uc_tokens_after_est": stats.get("tokens_after_est"),
                    "uc_kept": stats.get("kept_messages"),
                    "retried": retried,
                    "uc_savings_pct": stats.get("savings_pct"),
                    "uc_chars_per_token": stats.get("chars_per_token"),
                    "chained": chained,
                    "lineage_records": len(chain),
                }
            )

    # Aggregate table (numbers only).
    stock_ms = [s["stock_ms"] for s in samples if isinstance(s["stock_ms"], (int, float))]
    uc_ms = [s["uc_ms"] for s in samples if isinstance(s["uc_ms"], (int, float))]
    ratios = [s["stock_ms"] / max(s["uc_ms"], 1e-6) for s in samples
              if isinstance(s["stock_ms"], (int, float)) and isinstance(s["uc_ms"], (int, float)) and s["uc_ms"] > 0]
    pre = [s["preTokens"] for s in samples if isinstance(s["preTokens"], (int, float))]
    post = [s["postTokens"] for s in samples if isinstance(s["postTokens"], (int, float))]
    uc_after = [s["uc_tokens_after_est"] for s in samples if isinstance(s["uc_tokens_after_est"], (int, float))]
    cpt = [s["uc_chars_per_token"] for s in samples if isinstance(s["uc_chars_per_token"], (int, float))]
    tokens_avoided = sum(pre)

    aggregate = {
        "n": len(samples),
        "skipped_total": sum(skipped.values()),
        "skipped": skipped,
        "stock_ms": {"median": r1(med(stock_ms)), "p90": r1(pctl(stock_ms, 0.9))},
        "uc_ms": {"median": r1(med(uc_ms)), "p90": r1(pctl(uc_ms, 0.9))},
        "speedup_median_of_per_sample_ratios": r3(med(ratios)),
        "median_preTokens": r1(med(pre)),
        "median_postTokens_stock": r1(med(post)),
        "median_uc_tokens_after_est": r1(med(uc_after)),
        "summary_input_tokens_avoided": tokens_avoided,
        "retried_keepUserTurns0": sum(1 for s in samples if s["retried"]),
        "chained_compactions": sum(1 for s in samples if s["chained"]),
        "median_uc_chars_per_token": r3(med(cpt)),
    }

    caveats = [
        "UC token counts (uc_tokens_before_est/uc_tokens_after_est) are the binary's estimates (chars/token); "
        "preTokens/postTokens are Claude Code's own counts — comparisons mixing the two families are approximate. "
        "Marked by field name: *_est is estimated.",
        "stock_ms measures Claude Code compaction end-to-end, including its model call and post-processing; "
        "uc_ms measures only the local ultracompress subprocess (wall clock, perf_counter around subprocess.run).",
        "summary_input_tokens_avoided = sum(preTokens): stock compaction sends the whole context to the model once; "
        "UltraCompress is local and sends nothing. It is a prompt-token figure, not a cost figure.",
        "Lineage = non-sidechain records walked back via parentUuid from the boundary's logicalParentUuid, stopping "
        "at a previous compact boundary. For chained compactions (chained=true) the reconstructed input covers only "
        "the lineage since the earlier boundary while Claude's preTokens cover its full context, so those rows "
        "understate UltraCompress's input relative to what stock summarized.",
        "Older transcripts record compactions without compact_boundary records and are not covered by this benchmark.",
        "Retry policy mirrors the plugin: one keepUserTurns=0 rerun on 'no safe cut point' or no clean cut; retried "
        "samples report the retry's timing and stats.",
    ]

    print("UltraCompress vs Claude Code stock compaction (offline, no model calls)")
    print(f"binary: {bin_ver}  samples: {aggregate['n']}  skipped: {aggregate['skipped_total']} {skipped if skipped else ''}")
    if aggregate["n"]:
        print(f"stock compaction durationMs          : median {aggregate['stock_ms']['median']}  p90 {aggregate['stock_ms']['p90']}")
        print(f"ultracompress durationMs             : median {aggregate['uc_ms']['median']}  p90 {aggregate['uc_ms']['p90']}")
        print(f"speedup (median of stock_ms/uc_ms)   : {aggregate['speedup_median_of_per_sample_ratios']}x")
        print(f"preTokens (Claude, median)           : {aggregate['median_preTokens']}")
        print(f"postTokens stock (Claude, median)    : {aggregate['median_postTokens_stock']}")
        print(f"tokens_after_est (UC, est., median)  : {aggregate['median_uc_tokens_after_est']}")
        print(f"summary input tokens avoided         : {tokens_avoided} (sum preTokens)")
        print(f"retried keepUserTurns=0 / chained    : {aggregate['retried_keepUserTurns0']} / {aggregate['chained_compactions']}")
    print()
    for s in samples:
        sp = (s["stock_ms"] / s["uc_ms"]) if isinstance(s["stock_ms"], (int, float)) and s["uc_ms"] else None
        print(f"  {s['id']}  trig={s['trigger']}  model={s['model']}  v={s['version']}  "
              f"pre={s['preTokens']} post={s['postTokens']} uc_after={s['uc_tokens_after_est']}  "
              f"stock={s['stock_ms']}ms uc={s['uc_ms']}ms x={r3(sp)} kept={s['uc_kept']} "
              f"retry={int(bool(s['retried']))} chained={int(bool(s['chained']))}")
    print()
    for c in caveats:
        print(f"NOTE: {c}")

    if args.out:
        result = {
            "meta": {
                "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "binary_version": bin_ver,
                "binary": os.path.basename(args.bin),
                "transcripts_scanned": len(files),
                "transcripts_skipped_oversize": oversize,
                "limit": args.limit,
                "timeout_s": args.timeout,
                "max_file_bytes": args.max_file_bytes,
                "unparseable_lines": unparseable,
                "policy": "auto",
                "vision": "auto",
                "notes": caveats,
            },
            "aggregate": aggregate,
            "samples": samples,
        }
        out_dir = os.path.dirname(os.path.abspath(args.out))
        os.makedirs(out_dir, exist_ok=True)
        with open(args.out, "w", encoding="utf-8") as fh:
            json.dump(result, fh, indent=2)
            fh.write("\n")
        print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
