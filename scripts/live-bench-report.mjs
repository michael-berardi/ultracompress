#!/usr/bin/env node
/** Aggregate live-bench results: billed tokens, cost, compactions, correctness. */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

const RES = "/tmp/ultracompress-live-bench/results";
const SESSIONS = path.join(os.homedir(), ".pi", "agent", "sessions");
const TRUTH = { functions: "96", fatal: "E-8341-DEPLOY" }; // warn is intentionally ambiguous (no code= field)

function parseSessionStream(file) {
  let input = 0, cacheRead = 0, cacheWrite = 0, output = 0, cost = 0, compactions = 0, calls = 0, lastText = "";
  let ultracompressOwned = 0, frames = 0, ucPackets = 0;
  for (const line of fs.readFileSync(file, "utf8").split("\n")) {
    if (!line.trim()) continue;
    let v; try { v = JSON.parse(line); } catch { continue; }
    if (v.type === "compaction") {
      compactions++;
      if (JSON.stringify(v.details ?? {}).includes("ultracompress")) ultracompressOwned++;
    }
    const blob = line;
    if (blob.includes("[ultracompress:")) frames++;
    if (blob.includes("[UC packet")) ucPackets++;
    const msg = v.type === "message" ? v.message : v.type === "message_end" ? v.message : null;
    if (msg?.role === "assistant") {
      const u = v.message.usage;
      if (u) {
        calls++;
        input += u.input ?? 0;
        cacheRead += u.cacheRead ?? 0;
        cacheWrite += u.cacheWrite ?? 0;
        output += u.output ?? 0;
        cost += u.cost?.total ?? 0;
      }
      const c = v.message.content;
      if (Array.isArray(c)) {
        for (const b of c) if (b.type === "text" && b.text) lastText = b.text;
      }
    }
  }
  return { input, cacheRead, cacheWrite, output, cost, compactions, calls, lastText, ultracompressOwned, frames, ucPackets };
}

function answerOf(dir) {
  try {
    return fs.readFileSync(path.join(dir, "ANSWER.txt"), "utf8").trim();
  } catch {
    return null;
  }
}

function score(ans) {
  if (!ans) return { correct: false, note: "no ANSWER.txt" };
  const f = /functions=(\d+)/.exec(ans)?.[1];
  const fatal = /fatal=(\S+)/.exec(ans)?.[1];
  const functionsOk = f === TRUTH.functions;
  const fatalOk = fatal === TRUTH.fatal;
  return {
    correct: functionsOk && fatalOk,
    note: `functions ${f}${functionsOk ? "✓" : "✗(" + TRUTH.functions + ")"} fatal ${fatalOk ? "✓" : "✗(" + TRUTH.fatal + ")"}`,
  };
}

const runs = [];
for (const [name, wsDir, jsonl, secsFile] of [
  ["stock-pi", "/tmp/ultracompress-live-bench/ws-stock", path.join(RES, "stock.jsonl"), path.join(RES, "stock.secs")],
  ["ultracompress", "/tmp/ultracompress-live-bench/ws-ultracompress", path.join(RES, "ultracompress.jsonl"), path.join(RES, "ultracompress.secs")],
  ["omp-snapcompact", "/tmp/ultracompress-live-bench/ws-omp", path.join(RES, "omp.jsonl"), path.join(RES, "omp.secs")],
]) {
  // stock/UltraCompress: find the session in the sessions dir; omp: results jsonl IS the stream
  let file = jsonl;
  if (name !== "omp-snapcompact") {
    const tag = wsDir.replaceAll("/", "-");
    const dir = path.join(SESSIONS, `--${tag.replace(/^-/, "")}`.replace(/^-/, "--"));
    const d = fs.existsSync(dir) ? dir : Object.keys(0) && null;
    const found = fs
      .readdirSync(path.join(SESSIONS))
      .find((x) => x.includes("ultracompress-live-bench-ws") && x.endsWith(name === "stock-pi" ? "stock--" : "ultracompress--"));
    if (!found) { console.error(`no session dir for ${name}`); continue; }
    const files = fs.readdirSync(path.join(SESSIONS, found)).filter((f) => f.endsWith(".jsonl"));
    file = path.join(SESSIONS, found, files[files.length - 1]);
  }
  const m = parseSessionStream(file);
  const secs = parseInt(fs.readFileSync(secsFile, "utf8").trim(), 10);
  const ans = answerOf(wsDir);
  const sc = score(ans);
  runs.push({ name, ...m, secs, ans, ...sc });
}

const fmtN = (n) => n.toLocaleString("en-US");
const pct = (x) => `${x.toFixed(1)}%`;
console.log(`\nLIVE benchmark — identical task, identical model (glm-5.3-flash), ~23.6k compaction threshold\n`);
console.log(
  ["stack".padEnd(16), "billed-in".padStart(10), "(cache)".padStart(9), "out".padStart(7), "calls".padStart(6),
   "compactions".padStart(12), "cost".padStart(9), "wall".padStart(6), "correct"].join(" "),
);
const base = runs[0];
for (const r of runs) {
  const billedIn = r.input + r.cacheRead + r.cacheWrite;
  const rel = base && r !== base && base.billed > 0 ? ` (${pct((billedIn / base.billed - 1) * 100)})` : "";
  console.log(
    [
      r.name.padEnd(16),
      fmtN(billedIn).padStart(10),
      fmtN(r.cacheRead).padStart(9),
      fmtN(r.output).padStart(7),
      String(r.calls).padStart(6),
      `${r.compactions}${r.ultracompressOwned ? " (ultracompress:" + r.ultracompressOwned + ")" : ""}`.padStart(12),
      ("$" + r.cost.toFixed(4)).padStart(9),
      `${r.secs}s`.padStart(6),
      `${r.correct ? "✓" : "✗"} ${r.note}${rel}`,
    ].join(" "),
  );
  if (r.frames || r.ucPackets) console.log(`   └ ${r.name}: ${r.frames} frame markers · ${r.ucPackets} UC packet markers in context`);
}
console.log(
  "\nNote: billed-in includes cache reads (providers bill cached tokens at ~1/5 price). " +
  "Answer key: functions=96, fatal=E-8341-DEPLOY; warn= is graded generously (the WARN line has no code= field; " +
  "'NONE' or 'stale' both show the log was actually read).",
);
