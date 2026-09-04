#!/usr/bin/env node
/**
 * Offline benchmark: Rapid Compact vs stock Pi compaction vs VCC-only,
 * over real recorded Pi sessions. Deterministic, no API calls.
 *
 * Metric: the context size the NEXT LLM call would see after compaction
 * (summary + kept tail), calibrated per session against Pi's own measured
 * token usage (input + cacheRead + cacheWrite of the last assistant call).
 *
 * Modes compared:
 *   stock-pi   LLM summary (~1.2k tok est) + 20k-token verbatim tail,
 *              history destroyed (no recall)
 *   vcc-only   rc compact --policy vcc   (deterministic brief, no UC/snap)
 *   rc-auto    rc compact (auto) + live transforms on the kept tail
 *
 * Usage: node scripts/bench-offline.mjs [--rc path/to/rc] [--out docs/bench.json]
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
const OUT = argOf("--out", null);
const MIN_MESSAGES = 30; // only sessions big enough to ever compact
const MIN_RAW_TOKENS = parseInt(argOf("--min-raw", "40000"), 10); // compaction comparisons are meaningless below this
const STOCK_SUMMARY_TOKENS = 1200; // Pi's structured LLM summary, ~1.2k tokens
const STOCK_KEEP_RECENT = 20_000; // Pi default keepRecentTokens

const rc = (args_, stdin) =>
  JSON.parse(execFileSync(RC, args_, { input: stdin ? JSON.stringify(stdin) : undefined, encoding: "utf8" }));

function* sessionFiles() {
  const root = path.join(os.homedir(), ".pi", "agent", "sessions");
  if (!fs.existsSync(root)) return;
  for (const dir of fs.readdirSync(root)) {
    const d = path.join(root, dir);
    if (!fs.statSync(d).isDirectory()) continue;
    for (const f of fs.readdirSync(d)) {
      if (f.endsWith(".jsonl")) yield path.join(d, f);
    }
  }
}

function analyze(file) {
  const raw = fs.readFileSync(file, "utf8");
  const entries = [];
  let usage = null;
  for (const line of raw.split("\n")) {
    if (!line.trim()) continue;
    let v;
    try { v = JSON.parse(line); } catch { continue; }
    if (v.type === "message" || v.type === "compaction") entries.push(v);
    const u = v?.message?.usage ?? v?.usage;
    if (u && (u.input || u.cacheRead)) usage = u;
  }
  const messages = entries.filter((e) => e.type === "message");
  return { entries, messages, usage };
}

function charsOf(entry) {
  const m = entry.message ?? {};
  const c = m.content;
  if (typeof c === "string") return c.length;
  if (!Array.isArray(c)) return 0;
  let n = 0;
  for (const b of c) {
    if (typeof b?.text === "string") n += b.text.length;
    else if (b?.type === "toolCall") n += JSON.stringify(b.arguments ?? {}).length;
  }
  return n;
}

const fmt = (n) => n.toLocaleString("en-US");
const pct = (x) => `${x.toFixed(1)}%`;

const results = [];
for (const file of sessionFiles()) {
  try {
    const { entries, messages, usage } = analyze(file);
    if (messages.length < MIN_MESSAGES) continue;

    // Calibrated chars-per-token from Pi's own measured usage.
    const totalChars = messages.reduce((s, e) => s + charsOf(e), 0);
    const measuredTokens = usage ? (usage.input ?? 0) + (usage.cacheRead ?? 0) + (usage.cacheWrite ?? 0) : 0;
    const cpt = measuredTokens > 500 && totalChars > 1000 ? totalChars / measuredTokens : 3.8;
    const raw = measuredTokens > 500 ? measuredTokens : Math.round(totalChars / 3.8);

    // stock-pi: LLM summary + min(20k, tail). Tail = last user turn onward,
    // approximated by walking back until 20k tokens of content gathered.
    let tailChars = 0;
    let tailMsgs = 0;
    for (let i = messages.length - 1; i >= 0; i--) {
      const c = charsOf(messages[i]);
      if (tailChars + c > STOCK_KEEP_RECENT * cpt) break;
      tailChars += c;
      tailMsgs++;
    }
    const stockTail = Math.round(tailChars / cpt);
    const stockAfter = STOCK_SUMMARY_TOKENS + stockTail;

    // vcc-only
    const vcc = rc(["compact", "--policy", "vcc", "--vision", "off", "--keep-default"], {
      entries,
    });
    const vccAfter = vcc.stats.tokens_after_est;

    // rc-auto: compaction + live transforms on the kept tail
    const auto = rc(["compact", "--policy", "auto", "--vision", "on", "--keep-default"], {
      entries,
    });
    let tailAfter = auto.stats.tokens_after_est - 0; // includes kept tail untransformed
    // Recompute: tokens_after_est = summary + kept-tail. Isolate kept tail.
    const summaryTokens = Math.round(auto.summary.length / cpt);
    const keptTokens = Math.max(0, auto.stats.tokens_after_est - summaryTokens);
    let transformSaved = 0;
    if (keptTokens > 1000) {
      const keptId = auto.first_kept_entry_id;
      const idx = entries.findIndex((e) => e.id === keptId);
      const tailEntries = idx >= 0 ? entries.slice(idx) : [];
      if (tailEntries.length > 0) {
        try {
          const tr = rc(["transform", "--policy", "auto", "--vision", "on"], {
            messages: tailEntries,
            charsPerToken: cpt,
            modelVision: true,
          });
          transformSaved = tr.stats.tokens_before - tr.stats.tokens_after;
        } catch { /* transforms optional */ }
      }
    }
    const autoAfter = Math.max(summaryTokens, auto.stats.tokens_after_est - transformSaved);

    if (raw < MIN_RAW_TOKENS) continue; // below compaction territory
    results.push({
      file: path.basename(file).slice(0, 18),
      msgs: messages.length,
      cpt: +cpt.toFixed(2),
      raw,
      stockAfter,
      vccAfter,
      autoAfter,
      vccSavings: (1 - vccAfter / raw) * 100,
      autoSavings: (1 - autoAfter / raw) * 100,
      stockSavings: (1 - stockAfter / raw) * 100,
    });
  } catch (err) {
    console.error(`skip ${path.basename(file)}: ${String(err).slice(0, 120)}`);
  }
}

// Report
const avg = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
console.log(`\nOffline compaction benchmark — ${results.length} real sessions (calibrated chars/token from measured usage)\n`);
console.log(
  [
    "session".padEnd(20),
    "msgs".padStart(6),
    "raw tok".padStart(10),
    "stock-pi".padStart(10),
    "vcc-only".padStart(10),
    "rc-auto".padStart(10),
    "stock ↓".padStart(8),
    "vcc ↓".padStart(8),
    "rc-auto ↓".padStart(9),
  ].join(" "),
);
for (const r of results) {
  console.log(
    [
      r.file.padEnd(20),
      String(r.msgs).padStart(6),
      fmt(r.raw).padStart(10),
      fmt(r.stockAfter).padStart(10),
      fmt(r.vccAfter).padStart(10),
      fmt(r.autoAfter).padStart(10),
      pct(r.stockSavings).padStart(8),
      pct(r.vccSavings).padStart(8),
      pct(r.autoSavings).padStart(9),
    ].join(" "),
  );
}
console.log(
  "\nAVERAGE  stock-pi ↓",
  pct(avg(results.map((r) => r.stockSavings))),
  "· vcc-only ↓",
  pct(avg(results.map((r) => r.vccSavings))),
  "· rc-auto ↓",
  pct(avg(results.map((r) => r.autoSavings))),
);
console.log(
  "RECALL   stock-pi: none (history destroyed) · vcc-only: lossless · rc-auto: lossless\n",
);

if (OUT) {
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify({ results, averages: {
    stock: avg(results.map((r) => r.stockSavings)),
    vcc: avg(results.map((r) => r.vccSavings)),
    auto: avg(results.map((r) => r.autoSavings)),
  } }, null, 2));
  console.log(`wrote ${OUT}`);
}
