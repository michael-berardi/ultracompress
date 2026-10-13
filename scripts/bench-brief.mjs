#!/usr/bin/env node
/** Paired, frozen-corpus diagnostic comparison. No inference of error resolution.
 * --freeze DIR [--sessions ROOT] [--max 30] snapshots private Pi inputs (<=4MiB,
 * >=30 messages, >=160k content chars). Then --corpus DIR --baseline BIN
 * --candidate BIN [--out JSON]. Never commit private inputs or output excerpts.
 * Corpus changes are descriptive, not labelled diagnostic correctness proof.
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const args = process.argv.slice(2);
const arg = (name, fallback) => {
  const i = args.indexOf(name);
  if (i < 0) return fallback;
  if (!args[i + 1] || args[i + 1].startsWith('--')) throw new Error(`Missing value for ${name}`);
  return args[i + 1];
};
const digest = (data) => createHash('sha256').update(data).digest('hex');
const sections = (summary) => {
  const out = {};
  let key;
  for (const line of summary.split('\n')) {
    if (/^\[[^\]]+\]$/.test(line)) { key = line; out[key] ??= []; }
    else if (key && line.trim()) out[key].push(line);
  }
  return out;
};

try {
  const freeze = arg('--freeze', null);
  if (freeze) {
    const max = Number(arg('--max', '30'));
    if (!Number.isInteger(max) || max < 1 || max > 300) throw new Error('--max must be 1..300');
    if (fs.existsSync(freeze)) throw new Error('Snapshot directory already exists; refusing to overwrite frozen inputs');
    fs.mkdirSync(freeze, { recursive: true, mode: 0o700 });
    const root = arg('--sessions', path.join(os.homedir(), '.pi', 'agent', 'sessions'));
    const files = [];
    for (const dir of fs.readdirSync(root).sort()) {
      const dirPath = path.join(root, dir);
      if (!fs.statSync(dirPath).isDirectory() || dir.startsWith('__usap')) continue;
      for (const file of fs.readdirSync(dirPath).sort()) if (file.endsWith('.jsonl')) files.push(path.join(dirPath, file));
    }
    const manifest = { method: 'first qualifying sorted Pi session files; unlabelled private corpus', inputs: [], skippedOversize: 0, malformedLines: 0 };
    for (const file of files) {
      if (manifest.inputs.length >= max) break;
      if (fs.statSync(file).size > 4 * 1024 * 1024) { manifest.skippedOversize++; continue; }
      const entries = [];
      let chars = 0, messages = 0;
      for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
        if (!line.trim()) continue;
        let e;
        try { e = JSON.parse(line); } catch { manifest.malformedLines++; continue; }
        if (e.type !== 'message' && e.type !== 'compaction') continue;
        entries.push(e);
        if (e.type === 'message') { messages++; chars += JSON.stringify(e.message?.content ?? '').length; }
      }
      if (messages < 30 || chars < 160000) continue;
      const data = JSON.stringify({ entries });
      const name = `${String(manifest.inputs.length + 1).padStart(3, '0')}.json`;
      fs.writeFileSync(path.join(freeze, name), data, { mode: 0o600 });
      manifest.inputs.push({ name, sha256: digest(data), messages, chars });
    }
    if (!manifest.inputs.length) throw new Error('No qualifying sessions');
    fs.writeFileSync(path.join(freeze, 'manifest.json'), JSON.stringify(manifest, null, 2), { mode: 0o600 });
    console.log(`Frozen ${manifest.inputs.length} private inputs; skipped ${manifest.skippedOversize} files >4MiB.`);
  } else {
    const corpus = arg('--corpus', null), baseline = arg('--baseline', null), candidate = arg('--candidate', null);
    if (!corpus || !baseline || !candidate) throw new Error('Require --corpus DIR --baseline BIN --candidate BIN');
    const manifest = JSON.parse(fs.readFileSync(path.join(corpus, 'manifest.json'), 'utf8'));
    const run = (bin, data) => JSON.parse(execFileSync(bin, ['compact', '--policy', 'vcc', '--vision', 'off', '--keep-default'], {
      input: data, encoding: 'utf8', timeout: 10000, maxBuffer: 32 * 1024 * 1024,
    }));
    const result = { method: manifest.method, inputs: manifest.inputs.length, completed: 0, failed: 0, baselineFacts: 0, candidateFacts: 0, addedFacts: 0, removedFacts: 0, baselineSummaryChars: 0, candidateSummaryChars: 0, invariantFailures: [], rows: [] };
    for (const item of manifest.inputs) {
      if (!/^\d{3}\.json$/.test(item.name)) throw new Error('Invalid snapshot name');
      const data = fs.readFileSync(path.join(corpus, item.name), 'utf8');
      if (digest(data) !== item.sha256) throw new Error(`Snapshot hash mismatch: ${item.name}`);
      try {
        const b = run(baseline, data), c = run(candidate, data);
        const bs = sections(b.summary), cs = sections(c.summary);
        const bf = bs['[Key Facts]'] ?? [], cf = cs['[Key Facts]'] ?? [];
        const added = cf.filter(f => !bf.includes(f)).length, removed = bf.filter(f => !cf.includes(f)).length;
        delete bs['[Key Facts]']; delete cs['[Key Facts]'];
        if (JSON.stringify(bs) !== JSON.stringify(cs) || b.first_kept_entry_id !== c.first_kept_entry_id) result.invariantFailures.push(`${item.name}: non-diagnostic sections or kept-tail cut changed`);
        if (c.summary.length - b.summary.length > 2500) result.invariantFailures.push(`${item.name}: diagnostic growth exceeds 2500 chars`);
        result.completed++; result.baselineFacts += bf.length; result.candidateFacts += cf.length;
        result.addedFacts += added; result.removedFacts += removed;
        result.baselineSummaryChars += b.summary.length; result.candidateSummaryChars += c.summary.length;
        result.rows.push({ input: item.name, baselineFacts: bf.length, candidateFacts: cf.length, added, removed, baselineChars: b.summary.length, candidateChars: c.summary.length });
      } catch (error) {
        result.failed++;
        console.error(`${item.name}: paired comparison failed (${error.code ?? error.name}); excluded, not counted as success`);
      }
      console.error(`Compared ${result.completed + result.failed}/${result.inputs}`);
    }
    console.log(JSON.stringify(result, null, 2));
    const out = arg('--out', null);
    if (out) fs.writeFileSync(out, JSON.stringify(result, null, 2), { mode: 0o600 });
    if (result.failed || result.invariantFailures.length || !result.completed) process.exitCode = 1;
  }
} catch (error) {
  console.error(`Brief benchmark failed: ${error.message}`);
  process.exitCode = 1;
}
