#!/usr/bin/env node
/** Matched diagnostic-retention release gate. Synthetic fixtures, not a
 * production accuracy estimate. --baseline BIN --candidate BIN [--out JSON]
 */
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
const argv = process.argv.slice(2);
const arg = (key) => {
  const i = argv.indexOf(key);
  if (i < 0) return null;
  if (!argv[i + 1] || argv[i + 1].startsWith('--')) throw new Error(`Missing value for ${key}`);
  return argv[i + 1];
};
const section = (summary, name) => {
  const found = summary.split(`[${name}]\n`)[1];
  if (!found) return [];
  return found.split(/\n\[[^\]]+\]\n/)[0].split('\n').filter(l => l.startsWith('- ')).map(l => l.slice(2));
};
const withoutFacts = (summary) => summary.replace(/\[Key Facts\]\n[\s\S]*?(?=\n\[[^\]]+\]\n|$)/, '').replace(/\n{3,}/g, '\n\n');
const result = (text, tool = 'bash', error = false) => ({ text, tool, error });
const filler = 'building ordinary module successfully\n'.repeat(1400);
const cases = [
  { name: 'long-head-tail', results: [result(`E-8341-DEPLOY fatal rollback initiated\n${filler}E-8342-DEPLOY fatal rollback initiated\n`)], codes: ['E-8341-DEPLOY', 'E-8342-DEPLOY'] },
  { name: 'typescript', results: [result("src/error.ts:3:5 - error TS2304: Cannot find name 'foo'\n")], codes: ['TS2304'] },
  { name: 'npm', results: [result('npm ERR! code ELIFECYCLE\n')], codes: ['ELIFECYCLE'] },
  { name: 'explicit-code', results: [result('fatal: code=E_AUTH_FAIL\n')], codes: ['E_AUTH_FAIL'] },
  { name: 'unicode-boundaries', results: [result(`error TS1001: échec\n${'é'.repeat(17000)}\n${filler}warning code=W_UNICODE échec\n`)], codes: ['TS1001', 'W_UNICODE'] },
  { name: 'huge-single-line', results: [result('x'.repeat(1100000))], codes: [], empty: true },
  { name: 'huge-line-followed-by-error', results: [result(`${'x'.repeat(1100000)}\nerror TS1005: ';' expected\n`)], codes: ['TS1005'] },
  { name: 'source-negative', results: [result('const fake = "error TS2304: fixture code=E_FAKE";\n// error TS2305: code=E_COMMENT\nerror("code=E_CALL");\n', 'read')], codes: [], empty: true },
  { name: 'legacy', results: [result('E-8341-DEPLOY fatal rollback initiated\n')], codes: ['E-8341-DEPLOY'] },
  { name: 'legacy-log-read', results: [result('E-8341-DEPLOY fatal rollback initiated\nAUTH-123-FAIL fatal token rejected\n', 'read')], codes: ['E-8341-DEPLOY', 'AUTH-123-FAIL'] },
  { name: 'final-failure-after-warnings', results: [result(`warning code=W_FIRST\nwarning code=W_SECOND\nwarning code=W_THIRD\nwarning code=W_FOURTH\n${filler}fatal code=E_FINAL_FAILURE\n`)], codes: ['E_FINAL_FAILURE'] },
  { name: 'timestamped-log-read', results: [result('[2026-10-13T17:00:00Z] error code=E_TIMESTAMP\n[ERROR] code=E_LEVEL\n2026-10-13 17:00:00 error code=E_CLOCK\n', 'read')], codes: ['E_TIMESTAMP', 'E_LEVEL', 'E_CLOCK'] },
  { name: 'failed-json-diagnostic', results: [result('{"message":"fatal code=E_PARSE_FAIL"}\n', 'bash', true)], codes: ['E_PARSE_FAIL'] },
  { name: 'distinct-codes', results: [result('error TS2304: missing foo\nerror TS2305: missing bar\n')], codes: ['TS2304', 'TS2305'] },
  { name: 'dedup-per-result', results: [result('error TS2304: missing foo\n'.repeat(8) + 'error TS2305: missing bar\n')], codes: ['TS2304', 'TS2305'], facts: 2 },
  { name: 'ten-fact-budget', results: Array.from({ length: 14 }, (_, i) => result(`error TS${3000 + i}: missing name\n`)), codes: Array.from({ length: 10 }, (_, i) => `TS${3000 + i}`), facts: 10 },
];
const entriesFor = (fixture) => {
  const entries = [];
  const add = (message) => entries.push({ type: 'message', id: `m${entries.length}`, message });
  add({ role: 'user', content: [{ type: 'text', text: 'Fix compilation and retain exact diagnostics.\nAlways run tests before committing.' }] });
  fixture.results.forEach((r, i) => {
    add({ role: 'assistant', content: [{ type: 'toolCall', id: `t${i}`, name: r.tool, arguments: r.tool === 'read' ? { path: 'project/build.log' } : { command: 'npm run build' } }] });
    add({ role: 'toolResult', toolCallId: `t${i}`, toolName: r.tool, isError: r.error, content: [{ type: 'text', text: r.text }] });
  });
  add({ role: 'user', content: [{ type: 'text', text: 'Continue with the fix.' }] });
  return JSON.stringify({ entries });
};
try {
  const baseline = arg('--baseline'), candidate = arg('--candidate');
  if (!baseline || !candidate) throw new Error('Require --baseline BIN --candidate BIN');
  const run = (binary, input) => JSON.parse(execFileSync(binary, ['compact', '--policy', 'vcc', '--vision', 'off', '--keep-user-turns', '1', '--no-smart-keep'], { input, encoding: 'utf8', timeout: 10000, maxBuffer: 8 * 1024 * 1024 }));
  const report = { fixtures: cases.length, intendedCodes: 0, baselineKept: 0, candidateKept: 0, baselineFalseFacts: 0, candidateFalseFacts: 0, baselineChars: 0, candidateChars: 0, failures: [], rows: [] };
  for (const fixture of cases) {
    const input = entriesFor(fixture);
    const b = run(baseline, input), c = run(candidate, input);
    const bf = section(b.summary, 'Key Facts'), cf = section(c.summary, 'Key Facts');
    const carries = (fact, code) => {
      const escaped = code.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      return new RegExp(`(?<![A-Za-z0-9_-])${escaped}(?![A-Za-z0-9_-])`).test(fact);
    };
    const count = (facts) => fixture.codes.filter(code => facts.some(f => carries(f, code))).length;
    const bk = count(bf), ck = count(cf);
    report.intendedCodes += fixture.codes.length; report.baselineKept += bk; report.candidateKept += ck;
    report.baselineChars += b.summary.length; report.candidateChars += c.summary.length;
    if (fixture.empty) { report.baselineFalseFacts += bf.length; report.candidateFalseFacts += cf.length; }
    if (ck !== fixture.codes.length) report.failures.push(`${fixture.name}: missing intended code(s): ${fixture.codes.filter(code => !cf.some(f => carries(f, code))).join(', ')}`);
    if (fixture.empty && cf.length) report.failures.push(`${fixture.name}: false diagnostic fact(s)`);
    if (cf.length > 10 || cf.some(f => Buffer.byteLength(f, 'utf8') > 240)) report.failures.push(`${fixture.name}: fact count/length budget exceeded`);
    if (fixture.facts !== undefined && cf.length !== fixture.facts) report.failures.push(`${fixture.name}: expected ${fixture.facts} distinct facts, got ${cf.length}`);
    if (withoutFacts(b.summary) !== withoutFacts(c.summary) || b.first_kept_entry_id !== c.first_kept_entry_id) report.failures.push(`${fixture.name}: non-diagnostic context/cut changed`);
    if (c.summary.length - b.summary.length > 2500) report.failures.push(`${fixture.name}: >2500 summary characters added`);
    report.rows.push({ fixture: fixture.name, intended: fixture.codes.length, baselineKept: bk, candidateKept: ck, baselineFacts: bf.length, candidateFacts: cf.length, addedChars: c.summary.length - b.summary.length });
  }
  if (report.rows.find(r => r.fixture === 'long-head-tail').baselineKept !== 0 || report.rows.find(r => r.fixture === 'typescript').baselineKept !== 0) report.failures.push('Baseline does not exhibit predeclared long-output/TypeScript defects; wrong baseline binary');
  console.log(JSON.stringify(report, null, 2));
  const out = arg('--out');
  if (out) fs.writeFileSync(out, JSON.stringify(report, null, 2), { mode: 0o600 });
  if (report.failures.length) process.exitCode = 1;
} catch (error) {
  console.error(`Diagnostic gate failed: ${error.message}`);
  process.exitCode = 1;
}
