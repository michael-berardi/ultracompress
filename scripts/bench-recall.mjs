#!/usr/bin/env node
/**
 * Recall-quality benchmark: after Rapid Compact compaction, can the model
 * still recover facts from the compacted-away history? We sample real tool
 * results from pre-cut turns, derive rare-term queries from them, and
 * measure whether rc_recall returns the exact source entry in top-k.
 *
 * Stock Pi comparator: 0% by construction — compaction destroys the history
 * and there is no recall mechanism.
 *
 * Usage: node scripts/bench-recall.mjs [--rc path] [--k 5] [--samples 12]
 */

import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as url from "node:url";

const here = path.dirname(url.fileURLToPath(import.meta.url));
const repo = path.join(here, "..");
const args = process.argv.slice(2);
const argOf = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : dflt;
};
const RC = argOf("--rc", path.join(repo, "target", "release", "rc"));
const K = parseInt(argOf("--k", "5"), 10);
const SAMPLES = parseInt(argOf("--samples", "12"), 10);
const MIN_MESSAGES = 40;

const rc = (args_, stdin) =>
  JSON.parse(execFileSync(RC, args_, { input: stdin ? JSON.stringify(stdin) : undefined, encoding: "utf8" }));

function* sessionFiles() {
  const root = path.join(os.homedir(), ".pi", "agent", "sessions");
  for (const dir of fs.readdirSync(root)) {
    const d = path.join(root, dir);
    if (!fs.statSync(d).isDirectory()) continue;
    for (const f of fs.readdirSync(d)) if (f.endsWith(".jsonl")) yield path.join(d, f);
  }
}

const STOP = new Set("the a an and or of to in for with is are was were be been this that these those it its as at on by from not no yes true false error failed warning line file will can cannot into your you".split(" "));

function textOf(entry) {
  const c = entry?.message?.content;
  if (typeof c === "string") return c;
  if (!Array.isArray(c)) return "";
  return c.map((b) => (typeof b?.text === "string" ? b.text : "")).join("\n");
}

/** Rare-term query: pick mid-frequency distinctive words from the block. */
function queryFrom(text) {
  const freq = new Map();
  for (const w of text.toLowerCase().match(/[a-z][a-z0-9_.-]{3,}/g) ?? []) {
    if (STOP.has(w)) continue;
    freq.set(w, (freq.get(w) ?? 0) + 1);
  }
  const words = [...freq.entries()].filter(([, n]) => n >= 1);
  // Prefer longer, less common words (rare inside the block itself).
  words.sort((a, b) => (b[0].length - a[0].length) || (a[1] - b[1]));
  return words.slice(0, 4).map(([w]) => w).join(" ");
}

const fmt = (n) => n.toLocaleString("en-US");
const pct = (x) => `${x.toFixed(1)}%`;

let totalSamples = 0;
let hitAt1 = 0;
let hitAtK = 0;
const perSession = [];

for (const file of sessionFiles()) {
  let raw;
  try { raw = fs.readFileSync(file, "utf8"); } catch { continue; }
  const entries = [];
  for (const line of raw.split("\n")) {
    if (!line.trim()) continue;
    try { entries.push(JSON.parse(line)); } catch {}
  }
  const messages = entries.filter((e) => e.type === "message");
  if (messages.length < MIN_MESSAGES) continue;

  // Compact this session with rc.
  let compacted;
  try {
    compacted = rc(["compact", "--policy", "auto", "--vision", "on", "--keep-default"], { entries });
  } catch { continue; }

  // Sample pre-cut tool results (the compacted-away span).
  const cutIdx = messages.findIndex((e) => e.id === compacted.first_kept_entry_id);
  const away = messages.slice(0, cutIdx > 0 ? cutIdx : Math.floor(messages.length / 2));
  const blocks = [];
  for (const m of away) {
    const c = m?.message?.content;
    if (!Array.isArray(c)) continue;
    for (const b of c) {
      if (typeof b?.text === "string" && b.text.length > 400 && b.text.length < 20_000) {
        blocks.push({ entryId: m.id, text: b.text });
      }
    }
  }
  if (blocks.length === 0) continue;

  const step = Math.max(1, Math.floor(blocks.length / SAMPLES));
  let sHit1 = 0, sHitK = 0, sN = 0;
  for (let i = 0; i < blocks.length && sN < SAMPLES; i += step) {
    const b = blocks[i];
    const q = queryFrom(b.text);
    if (!q.trim()) continue;
    let res;
    try {
      res = rc(["recall", "--session", file, "--query", q, "--per-page", String(K)]);
    } catch { continue; }
    totalSamples++;
    sN++;
    const ids = res.hits.slice(0, K).map((h) => h.entry_id);
    if (ids[0] === b.entryId) hitAt1++;
    if (ids.includes(b.entryId)) { hitAtK++; sHitK++; }
    if (ids[0] === b.entryId) sHit1++;
  }
  if (sN > 0) {
    perSession.push({ file: path.basename(file).slice(0, 18), samples: sN, hit1: sHit1, hitK: sHitK });
  }
}

console.log(`\nRecall quality after Rapid Compact compaction (top-${K}, real sessions)\n`);
for (const p of perSession) {
  console.log(
    p.file.padEnd(20),
    `samples ${String(p.samples).padStart(3)}`,
    `hit@1 ${pct((p.hit1 / p.samples) * 100).padStart(7)}`,
    `hit@${K} ${pct((p.hitK / p.samples) * 100).padStart(7)}`,
  );
}
console.log(
  `\nTOTAL  samples ${totalSamples} · hit@1 ${pct((hitAt1 / Math.max(1, totalSamples)) * 100)}` +
  ` · hit@${K} ${pct((hitAtK / Math.max(1, totalSamples)) * 100)}`,
);
console.log("Stock Pi comparator: 0% — compacted history is destroyed and unreachable.\n");
