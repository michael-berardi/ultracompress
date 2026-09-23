/**
 * UltraCompress compaction — claude plugin tests (engine's own host).
 * Covers: entry mapping with tool calls, cut adjustment around tool results,
 * failure -> next(e) (stock compaction), binary candidate resolution,
 * precompute skip, the success message shape with kept handles preserved, and
 * the direct-binary recall contract (transcript resolution, default scope all,
 * readable failures).
 */
import { expect, test } from 'claude-code/testing';
import {
  boundedRecallText,
  capSummaryUtf8,
  chooseCut,
  firstKeptIndexFromId,
  formatCompactNotice,
  handleCompact,
  handleRecallCall,
  recallArgvFrom,
  register,
  toEntries,
  transcriptPathFor,
  ultraCompressBin,
} from './ultracompress.mjs';

type Any$ = Record<string, any>;

/** A $ with env/fs/process/ui faked in memory; ui.log lines are collected. */
function mock$ (opts: {
  home?: string | undefined;
  stat?: unknown | ((path: string) => unknown);
  env?: Record<string, string>;
  sessionId?: string;
  cwd?: string;
  run?: (argv: readonly string[], init: any) => Promise<{ exitCode: number; stdout: string; stderr: string }>;
} = {}) {
  const logs: string[] = [];
  const $$: Any$ = {
    env: { get: async (name: string) => (name === 'HOME' ? opts.home : opts.env?.[name]) },
    fs: { stat: async (path: string) => (typeof opts.stat === 'function' ? (opts.stat as any)(path) : opts.stat) },
    process: { run: opts.run ?? (async () => ({ exitCode: 0, stdout: '', stderr: '' })) },
    session: { id: async () => opts.sessionId, cwd: async () => opts.cwd },
    ui: { log: (text: string) => { logs.push(text); } },
  };
  return { $: $$, logs };
}

/** user -> assistant(toolUse) -> user(toolResult) -> user -> assistant -> user(toolResult). */
function transcript () {
  return [
    { role: 'user', text: 'inspect the directory', toolUses: [], handle: 'h0' },
    {
      role: 'assistant',
      text: 'Looking now.',
      toolUses: [{ tool_use_id: 't1', tool: 'Bash', input: { command: 'ls' }, text: 'a.txt' }],
      handle: 'h1',
    },
    {
      role: 'user',
      text: '',
      toolUses: [],
      toolResults: [{ tool_use_id: 't1', text: 'a.txt\nb.txt', isError: false }],
      handle: 'h2',
    },
    { role: 'user', text: 'now summarize', toolUses: [], handle: 'h3' },
    { role: 'assistant', text: 'Two files: a.txt, b.txt.', toolUses: [], handle: 'h4' },
    {
      role: 'user',
      text: '',
      toolUses: [],
      toolResults: [{ tool_use_id: 't9', text: 'stale', isError: false }],
      handle: 'h5',
    },
  ];
}

const RC = {
  summary: 'Goal: inspect the directory; found a.txt and b.txt; user asked to summarize.',
  first_kept_entry_id: 'm2',
  details: { compactor: 'ultracompress' },
  stats: {
    tokens_before_est: 182000,
    tokens_after_est: 24000,
    savings_pct: 86.8,
    summarized_messages: 3,
    kept_messages: 3,
    keep_user_turns_resolved: 2,
    smart_keep_adjusted: false,
    uc_blocks: 0,
    snap_blocks: 0,
    chars_per_token: 4,
    calibrated: false,
  },
  uc_status: { available: true },
};

test('toEntries maps text, tool calls and tool results into Pi entries with stable ids', () => {
  const entries = toEntries(transcript());
  expect(entries).toHaveLength(6); // m0, m1, m1:r0 (result entry), m3, m4, m5:r0
  expect(entries[0]).toEqual({
    type: 'message',
    id: 'm0',
    message: { role: 'user', content: [{ type: 'text', text: 'inspect the directory' }] },
  });
  expect(entries[1]).toEqual({
    type: 'message',
    id: 'm1',
    message: {
      role: 'assistant',
      content: [
        { type: 'text', text: 'Looking now.' },
        { type: 'toolCall', id: 't1', name: 'Bash', arguments: { command: 'ls' } },
      ],
    },
  });
  // The tool_result rides its own entry, attached to the message that carries it
  // (index 2 in this fixture), tool name resolved from the earlier tool_use.
  expect(entries[2]).toEqual({
    type: 'message',
    id: 'm2:r0',
    message: {
      role: 'toolResult',
      toolCallId: 't1',
      toolName: 'Bash',
      content: [{ type: 'text', text: 'a.txt\nb.txt' }],
      isError: false,
    },
  });
  expect(firstKeptIndexFromId('m2')).toBe(2);
  expect(firstKeptIndexFromId('m2:r1')).toBe(2);
  expect(firstKeptIndexFromId('entries')).toBe(-1);
});

test('chooseCut moves the cut to a clean user message and never splits tool_use from tool_result', () => {
  const messages = transcript();
  // Binary keeps from m2 (a tool-result user message): advance to m3.
  expect(chooseCut(messages, 2)).toBe(3);
  // Binary keeps from m1 (assistant): advance past the paired result to m3.
  expect(chooseCut(messages, 1)).toBe(3);
  // Binary keeps from m4 (assistant, then a tool-result user message): no valid cut.
  expect(chooseCut(messages, 4)).toBe(-1);
  // Already a clean user message: unchanged.
  expect(chooseCut(messages, 3)).toBe(3);
  // Past the end or empty: no cut.
  expect(chooseCut(messages, 99)).toBe(-1);
  expect(chooseCut([], 0)).toBe(-1);
});

test('missing binary, bad output, or empty summary degrade to stock compaction via next(e)', async () => {
  const messages = transcript();
  const nextCalls: unknown[] = [];
  const next = async (e: unknown) => { nextCalls.push(e); return { messages: ['stock'], via: 'next' }; };

  // Binary missing on disk.
  const missing = mock$({ home: '/home/x', stat: { kind: 'other', size: 0, mtimeMs: 0, isLink: false } });
  const outMissing = await handleCompact(missing.$, { trigger: 'auto', messages }, next);
  expect(outMissing).toEqual({ messages: ['stock'], via: 'next' });
  expect(nextCalls).toHaveLength(1);
  expect(missing.logs.join('\n')).toContain('binary missing');

  // Binary exits non-zero.
  const failed = mock$({
    home: '/home/x',
    stat: { kind: 'file', size: 1, mtimeMs: 0, isLink: false },
    run: async () => ({ exitCode: 1, stdout: '', stderr: 'uc: bad payload' }),
  });
  const outFailed = await handleCompact(failed.$, { trigger: 'auto', messages }, next);
  expect(outFailed).toEqual({ messages: ['stock'], via: 'next' });
  expect(failed.logs.join('\n')).toContain('uc: bad payload');

  // Unparseable stdout.
  const junk = mock$({
    home: '/home/x',
    stat: { kind: 'file', size: 1, mtimeMs: 0, isLink: false },
    run: async () => ({ exitCode: 0, stdout: 'not json', stderr: '' }),
  });
  const outJunk = await handleCompact(junk.$, { trigger: 'auto', messages }, next);
  expect(outJunk).toEqual({ messages: ['stock'], via: 'next' });

  // Empty summary.
  const empty = mock$({
    home: '/home/x',
    stat: { kind: 'file', size: 1, mtimeMs: 0, isLink: false },
    run: async () => ({ exitCode: 0, stdout: JSON.stringify({ ...RC, summary: '   ' }), stderr: '' }),
  });
  const outEmpty = await handleCompact(empty.$, { trigger: 'auto', messages }, next);
  expect(outEmpty).toEqual({ messages: ['stock'], via: 'next' });

  // Every fallback handed the untouched event to next.
  expect(nextCalls.every((e: any) => e.trigger === 'auto' && e.messages === messages)).toBe(true);
});

test('precompute answers skip without any work', async () => {
  const { $ } = mock$({ home: '/home/x' });
  let nextCalled = 0;
  const next = async () => { nextCalled += 1; return {}; };
  const out = await handleCompact($, { trigger: 'precompute', messages: [] }, next);
  expect(out).toEqual({ skip: 'UltraCompress compacts at the real trigger' });
  expect(nextCalled).toBe(0);
});

test('success hands the summary message first, keeps handles, and reports honest token counts', async () => {
  const messages = transcript();
  let seen: { argv: readonly string[]; init: any } | undefined;
  const { $, logs } = mock$({
    home: '/home/x',
    stat: { kind: 'file', size: 1, mtimeMs: 0, isLink: false },
    run: async (argv, init) => {
      seen = { argv, init };
      return { exitCode: 0, stdout: JSON.stringify(RC), stderr: '' };
    },
  });
  const next = async () => { throw new Error('next must not run on success'); };
  const out: any = await handleCompact($, { trigger: 'manual', messages }, next);

  // The binary was called at the first existing candidate with the compact contract.
  expect(seen!.argv).toEqual(['/home/x/.local/bin/ultracompress', 'compact', '--policy', 'auto', '--vision', 'auto']);
  const stdin = JSON.parse(seen!.init.stdin);
  expect(stdin.policy).toBe('auto');
  expect(stdin.smartKeepTail).toBe(true);
  expect(stdin.vision).toBe('auto');
  expect(stdin.entries.some((en: any) => en.message?.role === 'toolResult')).toBe(true);

  // m2 is a tool-result user message, so the kept tail starts at the next clean
  // user message (m3); the summary message is built, kept messages keep handles.
  expect(out.messages).toHaveLength(4); // summary + m3 + m4 + m5
  expect(out.messages[0].role).toBe('user');
  expect(out.messages[0].toolUses).toEqual([]);
  expect(out.messages[0].handle).toBeUndefined();
  expect(out.messages[0].text).toContain(
    'UltraCompress summary of the earlier conversation (3 messages condensed; ' +
    'recall archived detail with the ultracompress_recall tool)',
  );
  expect(out.messages[0].text).toContain(RC.summary);
  expect(out.messages[1]).toBe(messages[3]);
  expect(out.messages[2]).toBe(messages[4]);
  expect(out.messages[3]).toBe(messages[5]);
  expect(out.messages[1].handle).toBe('h3');
  expect(out.messages[2].handle).toBe('h4');
  expect(out.messages[3].handle).toBe('h5');
  expect(out.tokensBefore).toBe(182000);
  expect(out.tokensAfter).toBe(24000);
  expect(logs.join('\n')).toContain('UltraCompress · 182k → 24k tokens (−87%) · kept 3 turns');
});

test('header counts condensed messages (the cut), not the kept tail', async () => {
  // 9 rows with the binary keeping from m2: cut lands on m3, so 3 rows are
  // summarized and 6 are kept — the header must say 3, not 6.
  const messages = [
    ...transcript(),
    { role: 'user', text: 'carry one', toolUses: [], handle: 'h6' },
    { role: 'assistant', text: 'one', toolUses: [], handle: 'h7' },
    { role: 'user', text: 'carry two', toolUses: [], handle: 'h8' },
  ];
  const { $ } = mock$({
    home: '/home/x',
    stat: { kind: 'file', size: 1, mtimeMs: 0, isLink: false },
    run: async () => ({ exitCode: 0, stdout: JSON.stringify(RC), stderr: '' }),
  });
  const out: any = await handleCompact($, { trigger: 'manual', messages }, async () => ({}));
  expect(out.messages).toHaveLength(7); // summary + kept m3..m8
  expect(out.messages[0].text).toContain(
    'UltraCompress summary of the earlier conversation (3 messages condensed; ' +
    'recall archived detail with the ultracompress_recall tool)',
  );
});

test('$ULTRACOMPRESS_BIN overrides the candidate walk for compaction', async () => {
  let bin = '';
  const { $ } = mock$({
    home: '/home/x',
    stat: { kind: 'file', size: 1, mtimeMs: 0, isLink: false },
    env: { ULTRACOMPRESS_BIN: '/custom/ultracompress' },
    run: async (argv) => {
      bin = String(argv[0]);
      return { exitCode: 0, stdout: JSON.stringify(RC), stderr: '' };
    },
  });
  await handleCompact($, { trigger: 'auto', messages: transcript() }, async () => ({}));
  expect(bin).toBe('/custom/ultracompress');
});

test('binary resolution: first existing candidate wins, no override beats it', async () => {
  const FILE = { kind: 'file' as const, size: 1, mtimeMs: 0, isLink: false };
  // Override wins over everything on disk.
  const over = mock$({ stat: () => FILE, env: { ULTRACOMPRESS_BIN: '/custom/ultracompress' } });
  expect(await ultraCompressBin(over.$)).toBe('/custom/ultracompress');
  // First candidate that stats as a regular file wins.
  const partial = mock$({
    home: '/home/x',
    stat: (path: string) => (path === '/opt/homebrew/bin/ultracompress' ? FILE : undefined),
  });
  expect(await ultraCompressBin(partial.$)).toBe('/opt/homebrew/bin/ultracompress');
  // Nothing exists: the first candidate is returned so the failure names a path.
  const none = mock$({ home: '/home/x', stat: () => undefined });
  expect(await ultraCompressBin(none.$)).toBe('/home/x/.local/bin/ultracompress');
});

test('manual instructions keep:/policy: steer the binary; formatCompactNotice is exact', async () => {
  const messages = transcript();
  let policy = '';
  const { $ } = mock$({
    home: '/home/x',
    stat: { kind: 'file', size: 1, mtimeMs: 0, isLink: false },
    run: async (_argv, init) => {
      policy = JSON.parse(init.stdin).policy;
      return { exitCode: 0, stdout: JSON.stringify({ ...RC, first_kept_entry_id: 'm3' }), stderr: '' };
    },
  });
  const next = async () => ({});
  await handleCompact($, { trigger: 'manual', instructions: 'keep:2 policy:vcc focus on the todo list', messages }, next);
  expect(policy).toBe('vcc');
  expect(formatCompactNotice(RC.stats)).toBe('UltraCompress · 182k → 24k tokens (−87%) · kept 3 turns');
});

test('summary cap and recall argv obey their byte/parameter bounds', () => {
  // Cap keeps whole characters and appends the recovery route.
  const capped = capSummaryUtf8('é'.repeat(20000), 16384);
  expect(new TextEncoder().encode(capped).length).toBeLessThanOrEqual(16384);
  expect(capped).toContain('[Summary capped; use ultracompress_recall for omitted history.]');
  expect(capSummaryUtf8('short', 16384)).toBe('short');

  const argv = recallArgvFrom(
    { query: 'parser bug', scope: 'lineage', regex: true, role: 'toolResult', page: 2, perPage: 10, snippetBytes: 512, maxOutputBytes: 8000 },
    '/tmp/s.jsonl',
  );
  expect(argv).toEqual([
    'recall', '--session', '/tmp/s.jsonl', '--format', 'claude', '--query', 'parser bug', '--scope', 'lineage',
    '--regex', '--role', 'toolResult', '--page', '2', '--per-page', '10',
    '--snippet-bytes', '512', '--max-output-bytes', '8000',
  ]);
  // Default scope all: one session, every branch, including history from
  // before compaction (lineage alone stops at a compaction boundary).
  expect(recallArgvFrom({ query: 'q' }, '/tmp/s.jsonl')).toEqual([
    'recall', '--session', '/tmp/s.jsonl', '--format', 'claude', '--query', 'q', '--scope', 'all', '--snippet-bytes', '4000',
  ]);
  expect(() => recallArgvFrom({ query: '' }, '/tmp/s.jsonl')).toThrow(/query/);
  expect(() => recallArgvFrom({ query: 'x', nope: 1 }, '/tmp/s.jsonl')).toThrow(/Unknown recall option/);
  expect(() => recallArgvFrom({ query: 'x' }, undefined)).toThrow(/session file/i);
  expect(() => boundedRecallText('x'.repeat(13000))).toThrow(/byte budget/);
  expect(boundedRecallText('{"ok":true}')).toBe('{"ok":true}');

  // The transcript path Claude Code itself writes, with CLAUDE_CONFIG_DIR and without.
  expect(transcriptPathFor('/cfg/claude', '/home/x', '/tmp/My Dir', 's-1')).toBe('/cfg/claude/projects/-tmp-My-Dir/s-1.jsonl');
  expect(transcriptPathFor(undefined, '/home/x', '/tmp/My Dir', 's-1')).toBe('/home/x/.claude/projects/-tmp-My-Dir/s-1.jsonl');
  expect(transcriptPathFor('   ', '/home/x', '/home/x/proj (v2)', 'abc')).toBe('/home/x/.claude/projects/-home-x-proj--v2-/abc.jsonl');
});

test('recall tool.call runs the binary directly on the resolved transcript', async () => {
  const FILE = { kind: 'file' as const, size: 1, mtimeMs: 0, isLink: false };
  const runCalls: Array<{ argv: readonly string[]; init: any }> = [];
  const run = async (argv: readonly string[], init: any) => {
    runCalls.push({ argv, init });
    return { exitCode: 0, stdout: '{"total":1}', stderr: '' };
  };

  // Explicit sessionFile; the binary comes from $ULTRACOMPRESS_BIN.
  const explicit = mock$({ stat: () => FILE, run, env: { ULTRACOMPRESS_BIN: '/opt/uc/bin/ultracompress' } });
  const outExplicit: any = await handleRecallCall(explicit.$, { sessionFile: '/t/other.jsonl', query: 'lost key' });
  expect(outExplicit).toEqual({ result: '{"total":1}' });
  expect(runCalls[0].argv).toEqual([
    '/opt/uc/bin/ultracompress', 'recall', '--session', '/t/other.jsonl', '--format', 'claude',
    '--query', 'lost key', '--scope', 'all', '--snippet-bytes', '4000',
  ]);

  // Default target: this session's transcript under $CLAUDE_CONFIG_DIR,
  // stat-checked as a regular file; the managed bridge serves when no override is set.
  const mine = mock$({
    home: '/home/x',
    stat: (path: string) =>
      String(path).endsWith('/ultracompress') || path === '/cfg/claude/projects/-tmp-My-Dir/s-1.jsonl' ? FILE : undefined,
    run,
    sessionId: 's-1',
    cwd: '/tmp/My Dir',
    env: { CLAUDE_CONFIG_DIR: '/cfg/claude' },
  });
  const outMine: any = await handleRecallCall(mine.$, { query: 'plan', scope: 'lineage' });
  expect(outMine.result).toBe('{"total":1}');
  expect(runCalls[1].argv.slice(0, 10)).toEqual([
    '/home/x/.local/bin/ultracompress', 'recall', '--session', '/cfg/claude/projects/-tmp-My-Dir/s-1.jsonl',
    '--format', 'claude', '--query', 'plan', '--scope', 'lineage',
  ]);
  expect(runCalls[1].init.env.UC_TEXT_ENVELOPES).toBe('1');
  expect(runCalls[1].init.env.CLAUDE_CONFIG_DIR).toBeUndefined();
  expect(runCalls[1].init.timeoutMs).toBe(30_000);

  // A failed binary run degrades to a readable failure, never a throw.
  const bad = mock$({ stat: () => FILE, run: async () => ({ exitCode: 2, stdout: '', stderr: 'uc: bad scope' }) });
  const outBad: any = await handleRecallCall(bad.$, { sessionFile: '/t/other.jsonl', query: 'x' });
  expect(outBad.result).toContain('UltraCompress recall failed: uc: bad scope');

  // Missing transcript: readable failure naming the path, no run.
  const gone = mock$({
    home: '/home/x',
    stat: (path: string) => (String(path).endsWith('/ultracompress') ? FILE : undefined),
    run,
    sessionId: 's-1',
    cwd: '/tmp/Gone Dir',
  });
  const outGone: any = await handleRecallCall(gone.$, { query: 'x' });
  expect(outGone.result).toContain("transcript is missing at /home/x/.claude/projects");
  expect(runCalls).toHaveLength(2);

  // No session id/cwd and no explicit file: readable failure, no run.
  const orphan = mock$({ home: '/home/x', stat: () => FILE, run });
  const outOrphan: any = await handleRecallCall(orphan.$, {});
  expect(outOrphan.result).toContain('No session file available');
  expect(runCalls).toHaveLength(2);

  // Missing binary: readable failure, no run.
  const noBin = mock$({
    home: '/home/x',
    stat: (path: string) => (String(path).endsWith('/ultracompress') ? undefined : FILE),
    run,
    sessionId: 's-1',
    cwd: '/tmp',
  });
  const outNoBin: any = await handleRecallCall(noBin.$, { query: 'x' });
  expect(outNoBin.result).toContain('binary missing at /home/x/.local/bin/ultracompress');
  expect(runCalls).toHaveLength(2);
});

test('engine wiring: the registered hooks answer session.compact over the engine chain', async ($, on) => {
  register(on, {});
  const out = await ($.session as any).compact({ trigger: 'precompute', messages: [] });
  expect(out).toEqual({ skip: 'UltraCompress compacts at the real trigger' });
});
