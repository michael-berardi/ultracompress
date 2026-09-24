/**
 * UltraCompress for Claude Code function hooks — compaction + recall.
 *
 * Replaces Claude's own compaction summarizer with the local UltraCompress
 * binary: a deterministic summary with no model call, plus a recall tool over
 * the raw transcript. This is the standalone, drop-in Claude Code plugin (the
 * UltraTerm bundle carries its own identical copy with a managed binary path).
 *
 * Binary contract (Pi parity; first regular file wins):
 *   $ULTRACOMPRESS_BIN, $HOME/.local/bin/ultracompress,
 *   $HOME/.ultraterm/bin/ultracompress, /opt/homebrew/bin/ultracompress,
 *   /usr/local/bin/ultracompress
 *   argv   compact --policy <auto|vcc|snap|uc> --vision <auto|on|off>
 *   stdin  { entries, tokensBefore?, previousSummary?, policy, keepUserTurns,
 *            smartKeepTail, vision, modelVision, ucBin, ucEnabled, ucMinChars,
 *            snapMinChars }
 *   stdout { summary, first_kept_entry_id, details, stats, uc_status }
 *
 * Recall contract: the binary reads Claude Code transcripts natively
 * (`recall --session <transcript.jsonl> --format claude`), so the recall tool
 * calls it directly on this session's transcript file. The hook resolves the
 * transcript itself: an explicit sessionFile, else
 * ${CLAUDE_CONFIG_DIR:-~/.claude}/projects/<cwd, every non [A-Za-z0-9] char as
 * '-'>/<session id>.jsonl, stat-checked as a regular file. The default scope is
 * all — one session, every branch, including history written before compac-
 * tions — because --scope lineage stops at a compaction boundary in a real
 * transcript. Its argv mirrors the tool's parameters:
 *   <binary> recall --session <transcript> --format claude --query <q>
 *            --scope <all|lineage> [--regex] [--role r] [--tool-name n]
 *            [--after-entry id] [--before-entry id] [--page n] [--per-page n]
 *            [--snippet-bytes n] [--max-output-bytes n]
 *
 * Entries are Pi message entries exactly as the Pi extension's claude_entries()
 * builds them from a Claude transcript:
 *   { type: 'message', id, parentId?, message: { role, content: [
 *       { type: 'text', text } | { type: 'toolCall', id, name, arguments } ] } }
 *   { type: 'message', id, message: { role: 'toolResult', toolCallId, toolName,
 *       content: [{ type: 'text', text }], isError } }
 *
 * Failure posture (Pi parity): every UltraCompress call is best-effort. If the
 * binary is missing, errors, times out, or yields no clean cut, the hook calls
 * next(e) and Claude's stock compaction runs. UltraCompress never bricks a
 * session. This module imports nothing: no Node, no DOM — everything via $.
 */

/** Message entries are identified as `m<index>`; a tool result entry is `m<index>:r<j>`. */
const ENTRY_ID_RE = /^m(\d+)(?::|$)/;

const COMPACT_DEFAULTS = {
  policy: 'auto',
  keepUserTurns: null,
  smartKeepTail: true,
  vision: 'auto',
  modelVision: null,
  ucBin: 'uc',
  ucEnabled: true,
  ucMinChars: 8192,
  snapMinChars: 8192,
};

/** Pi's compaction bridge kills the child at 30s; $.process.run's default is also 30s. */
const COMPACT_TIMEOUT_MS = 30_000;

/** Pi's recall bridge budget: complete result JSON byte cap (recallText). */
const RECALL_MAX_OUTPUT_BYTES = 12_000;

/** Pi caps automatic compaction summaries at summaryMaxBytes (default 16384 UTF-8 bytes). */
const SUMMARY_MAX_BYTES = 16_384;

/**
 * The registered recall tool, as plugin.json names the plugin (`ultracompress`).
 * Served by a tool.call hook on exactly this matcher string.
 */
const RECALL_TOOL = {
  name: 'ultracompress_recall',
  matcher: 'mcp__ultracompress__ultracompress_recall',
  description:
    "Search raw pre-compaction history before claiming context is lost. Defaults to this whole " +
    'session: every branch, including history from before compaction. scope:\'lineage\' narrows to ' +
    'the current branch since the last compaction. Never scans other sessions; supply sessionFile ' +
    'explicitly for one. Narrow by role, tool, entry range and bounded excerpts. ' +
    'Results identify the searched session, scope and message count; no automatic widening. ' +
    'Archived oversized tool outputs appear as [UC uc:<64hex>] markers; a marker reference is bounded session-local ' +
    "memory — when a marker's original text is no longer reachable, search for the text here by its distinctive " +
    'words instead of retrying or inventing a packet.',
  inputSchema: {
    type: 'object',
    properties: {
      query: { type: 'string', minLength: 1, maxLength: 512, description: 'Ranked keywords; use regex:true for an explicit regex.' },
      scope: { type: 'string', enum: ['lineage', 'all'], description: "Default: all — every branch of this one session, including history from before compaction. lineage = the current branch since the last compaction." },
      sessionFile: { type: 'string', description: 'Explicit other-session JSONL path. Omit to search only this session; never scans the session archive.' },
      regex: { type: 'boolean', description: 'Interpret query as a regular expression. Default false.' },
      role: { type: 'string', enum: ['user', 'assistant', 'toolResult'], description: 'Restrict message role before ranking.' },
      toolName: { type: 'string', description: 'Restrict to results from this exact tool name.' },
      afterEntry: { type: 'string', description: 'Search only after this entry ID in the selected scope (exclusive).' },
      beforeEntry: { type: 'string', description: 'Search only before this entry ID in the selected scope (exclusive).' },
      page: { type: 'integer', minimum: 1, maximum: 1000000, description: '1-based page. Default 1.' },
      perPage: { type: 'integer', minimum: 1, maximum: 20, description: 'Hits per page. Default 5.' },
      snippetBytes: { type: 'integer', minimum: 128, maximum: 4000, description: 'UTF-8 bytes per excerpt, not tokens. Default 4000 (below the 4096-byte per-hit cap).' },
      maxOutputBytes: { type: 'integer', minimum: 1024, maximum: 32000, description: 'Complete result JSON byte budget (transport wrapper excluded), not tokens. Default 12000.' },
    },
    required: ['query'],
    additionalProperties: false,
  },
};

const RECALL_INT_BOUNDS = {
  page: [1, 1000000],
  perPage: [1, 20],
  snippetBytes: [128, 4000],
  maxOutputBytes: [1024, 32000],
};

const RECALL_STRING_PARAMS = {
  role: ['user', 'assistant', 'toolResult'],
  toolName: null,
  afterEntry: null,
  beforeEntry: null,
};

/**
 * The binary path: $ULTRACOMPRESS_BIN when set, else the first of
 * ~/.local/bin/ultracompress, ~/.ultraterm/bin/ultracompress,
 * /opt/homebrew/bin/ultracompress, /usr/local/bin/ultracompress that stats as
 * a regular file (the engine's stat exposes no permission bits, so an existing
 * but non-executable file surfaces as a readable process.run failure, never a
 * silent one). When none exists, the first candidate is returned so the caller
 * fails once, naming a path — compaction falls back to Claude's stock
 * summarizer with that one log line.
 */
const BIN_CANDIDATES = (home) => [
  `${home}/.local/bin/ultracompress`,
  `${home}/.ultraterm/bin/ultracompress`,
  '/opt/homebrew/bin/ultracompress',
  '/usr/local/bin/ultracompress',
];

export async function ultraCompressBin($) {
  const override = await $.env.get('ULTRACOMPRESS_BIN');
  if (typeof override === 'string' && override.trim()) return override;
  const home = (await $.env.get('HOME')) ?? '';
  const candidates = BIN_CANDIDATES(home);
  for (const path of candidates) {
    const st = await $.fs.stat(path).catch(() => undefined);
    if (st && st.kind === 'file') return path;
  }
  return candidates[0];
}

/**
 * Map a session.compact transcript (SessionMessage rows) to the Pi message
 * entries the binary compacts — claude_entries()' exact shape, with stable ids
 * `m<index>` / `m<index>:r<j>` so first_kept_entry_id maps back to a message.
 */
export function toEntries(messages) {
  const entries = [];
  const names = new Map(); // tool_use_id -> tool name
  const rows = Array.isArray(messages) ? messages : [];
  rows.forEach((m, i) => {
    const id = `m${i}`;
    if (!m || (m.role !== 'assistant' && m.role !== 'user')) return;
    if (m.role === 'assistant') {
      const uses = Array.isArray(m.toolUses) ? m.toolUses : [];
      for (const u of uses) {
        if (u && u.tool_use_id) names.set(u.tool_use_id, typeof u.tool === 'string' ? u.tool : '');
      }
      const parts = [];
      if (typeof m.text === 'string' && m.text) parts.push({ type: 'text', text: m.text });
      for (const u of uses) {
        parts.push({
          type: 'toolCall',
          id: u.tool_use_id,
          name: typeof u.tool === 'string' ? u.tool : '',
          arguments: u.input && typeof u.input === 'object' ? u.input : {},
        });
      }
      if (parts.length) entries.push({ type: 'message', id, message: { role: 'assistant', content: parts } });
    } else {
      const parts = [];
      if (typeof m.text === 'string' && m.text) parts.push({ type: 'text', text: m.text });
      if (parts.length) entries.push({ type: 'message', id, message: { role: 'user', content: parts } });
      const results = Array.isArray(m.toolResults) ? m.toolResults : [];
      results.forEach((r, j) => {
        entries.push({
          type: 'message',
          id: `${id}:r${j}`,
          message: {
            role: 'toolResult',
            toolCallId: r && r.tool_use_id,
            toolName: r && r.tool_use_id && names.has(r.tool_use_id) ? names.get(r.tool_use_id) : '',
            content: [{ type: 'text', text: r && typeof r.text === 'string' ? r.text : '' }],
            isError: Boolean(r && r.isError),
          },
        });
      });
    }
  });
  return entries;
}

/** `/compact keep:2 policy:vcc rest…` -> { keep: 2, policy: 'vcc' } (Pi parseUltraCompressArgs). */
export function parseInstructions(raw) {
  const keepMatch = /(?:^|\s)keep:(\d+)(?=\s|$)/.exec(raw ?? '');
  const policyMatch = /(?:^|\s)policy:(auto|vcc|snap|uc)(?=\s|$)/.exec(raw ?? '');
  return {
    keep: keepMatch ? Math.max(0, parseInt(keepMatch[1], 10)) : null,
    policy: policyMatch ? policyMatch[1] : null,
  };
}

/** first_kept_entry_id `m12` / `m12:0` -> 12; anything else -> -1. */
export function firstKeptIndexFromId(id) {
  const match = typeof id === 'string' ? ENTRY_ID_RE.exec(id) : null;
  return match ? parseInt(match[1], 10) : -1;
}

/**
 * Move the binary's kept index forward to the first clean user message: role
 * user with no toolResults (a tool_result must never be split from the
 * tool_use the summary would swallow). Returns -1 when no valid cut exists.
 */
export function chooseCut(messages, keptIndex) {
  const rows = Array.isArray(messages) ? messages : [];
  let i = Math.max(0, Math.min(Number.isFinite(keptIndex) ? keptIndex : 0, rows.length));
  for (; i < rows.length; i++) {
    const m = rows[i];
    if (m && m.role === 'user' && !(Array.isArray(m.toolResults) && m.toolResults.length)) return i;
  }
  return -1;
}

/** Pure notice line, Steak Pi style: `UltraCompress · 182k → 24k tokens (−87%) · kept 6 turns`. */
export function formatCompactNotice(stats) {
  const s = stats && typeof stats === 'object' ? stats : {};
  const tok = (n) => {
    const v = typeof n === 'number' && Number.isFinite(n) ? Math.max(0, n) : 0;
    if (v >= 10000) return `${Math.round(v / 1000)}k`;
    if (v >= 1000) return `${(v / 1000).toFixed(1)}k`;
    return String(v);
  };
  const before = typeof s.tokens_before_est === 'number' && Number.isFinite(s.tokens_before_est) ? s.tokens_before_est : null;
  const after = typeof s.tokens_after_est === 'number' && Number.isFinite(s.tokens_after_est) ? s.tokens_after_est : null;
  // Signed change from the two estimates (a short session can grow slightly:
  // the brief has fixed sections), never a double sign.
  let change = '';
  if (before && after !== null) {
    const pct = Math.round(((after - before) / before) * 100);
    change = pct <= 0 ? ` (\u2212${Math.abs(pct)}%)` : ` (+${pct}%)`;
  }
  const parts = [`UltraCompress · ${tok(before)} \u2192 ${tok(after)} tokens${change}`];
  if (typeof s.kept_messages === 'number' && Number.isFinite(s.kept_messages)) parts.push(`kept ${s.kept_messages} messages`);
  return parts.join(' · ');
}

/** Hard UTF-8 budget (Pi capSummary) with a recall recovery route; never splits a character. */
export function capSummaryUtf8(summary, maxBytes) {
  const budget = Number.isFinite(maxBytes) ? Math.max(1024, Math.min(65536, Math.floor(maxBytes))) : 16384;
  const bytes = new TextEncoder().encode(summary);
  if (bytes.length <= budget) return summary;
  const marker = '\n[Summary capped; use ultracompress_recall for omitted history.]';
  let end = budget - new TextEncoder().encode(marker).length;
  if (end < 0) end = 0;
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--;
  return new TextDecoder().decode(bytes.subarray(0, end)) + marker;
}

/** The compact stdin payload (binary contract), defaults per the Pi settings. */
export function buildCompactStdin(messages, overrides) {
  const parsed = parseInstructions(overrides && overrides.instructions);
  const stdin = {
    ...COMPACT_DEFAULTS,
    entries: toEntries(messages),
    policy: parsed.policy ?? COMPACT_DEFAULTS.policy,
    keepUserTurns: parsed.keep ?? COMPACT_DEFAULTS.keepUserTurns,
  };
  if (overrides && typeof overrides.tokensBefore === 'number') stdin.tokensBefore = overrides.tokensBefore;
  if (overrides && typeof overrides.previousSummary === 'string') stdin.previousSummary = overrides.previousSummary;
  return stdin;
}

/** Telemetry opt-in parity with the Pi bridge (env overlays over the host environment). */
async function bridgeEnv($) {
  const env = { UC_TEXT_ENVELOPES: '1' };
  try {
    const tele = await $.env.get('UC_TELEMETRY');
    const path = await $.env.get('UC_TELEMETRY_PATH');
    if (tele === undefined && path === undefined) env.UC_TELEMETRY = '1';
  } catch {}
  return env;
}

function excerpt(text, max) {
  const line = String(text ?? '').trim();
  return line.length > max ? `${line.slice(0, max)}…` : line;
}

/**
 * session.compact hook: run the UltraCompress binary over the transcript and
 * hand up { messages } with the summary first; any miss degrades to next(e)
 * (Claude's stock compaction) and logs why.
 */
export async function handleCompact($, e, next) {
  const trigger = e && e.trigger;
  if (trigger === 'precompute') {
    return { skip: 'UltraCompress compacts at the real trigger' };
  }
  if (trigger !== 'manual' && trigger !== 'auto' && trigger !== 'plugin') return next(e);

  const fail = async (why) => {
    try {
      $.ui.log(`UltraCompress · stock compaction: ${excerpt(why, 120)}`);
      $.ui.log(`ultracompress compact fell back: ${why}`, { to: 'debug' });
    } catch {}
    return next(e);
  };
  try {
    const home = await $.env.get('HOME');
    if (!home) return fail('no HOME environment variable');
    const bin = await ultraCompressBin($);
    const st = await $.fs.stat(bin).catch(() => undefined);
    if (!st || st.kind !== 'file') return fail(`binary missing at ${bin}`);

    const messages = Array.isArray(e.messages) ? e.messages : [];
    if (!messages.length) return fail('no transcript messages to compact');

    const run = async (overrides) => {
      const stdin = { ...buildCompactStdin(messages, { instructions: e.instructions }), ...overrides };
      const res = await $.process.run([bin, 'compact', '--policy', stdin.policy, '--vision', stdin.vision], {
        stdin: JSON.stringify(stdin),
        env: await bridgeEnv($),
        timeoutMs: COMPACT_TIMEOUT_MS,
      });
      if (res.exitCode !== 0) return { error: `UltraCompress exited ${res.exitCode}: ${excerpt(res.stderr, 300)}`, noCut: /no safe cut point/.test(res.stderr ?? '') };
      try {
        return { rc: JSON.parse(res.stdout) };
      } catch (err) {
        return { error: `bad UltraCompress output: ${err}` };
      }
    };
    // A short or tool-heavy conversation can have no clean place to keep a
    // tail. Claude's own compaction then summarizes everything, so do the
    // same locally (keep no turns) rather than hand off to a model call.
    let attempt = await run({});
    let cut = -1;
    let reportedStats;
    if (!attempt.error) {
      const keptIndex = firstKeptIndexFromId(attempt.rc && attempt.rc.first_kept_entry_id);
      cut = keptIndex < 0 ? -1 : chooseCut(messages, keptIndex);
      if (cut > keptIndex) {
        // The binary summarized only the messages before its own cut. Moving
        // the tail forward to a clean user message would drop everything in
        // between (a tool result and any user text sent with it), so
        // summarize exactly the messages before the clean cut instead.
        reportedStats = attempt.rc && attempt.rc.stats;
        attempt = await run({ entries: toEntries(messages.slice(0, cut)), keepUserTurns: 0 });
      }
    }
    if ((attempt.error && attempt.noCut) || (!attempt.error && cut < 0)) {
      attempt = await run({ keepUserTurns: 0 });
      reportedStats = undefined;
      if (!attempt.error) cut = messages.length;
    }
    if (attempt.error) return fail(attempt.error);
    const rc = attempt.rc;
    // Token estimates describe the whole conversation; a prefix-only rerun
    // would understate them, so report the full run's estimates.
    if (reportedStats && typeof reportedStats === 'object') rc.stats = reportedStats;
    const summary = rc && typeof rc.summary === 'string' ? rc.summary : '';
    if (!summary.trim()) return fail('empty summary');
    if (cut < 0) return fail(`no clean user-message cut at or after entry ${rc.first_kept_entry_id}`);

    const condensed = cut; // summarized = everything before the kept tail (not the kept count)
    const header =
      `UltraCompress summary of the earlier conversation (${condensed} messages condensed; ` +
      'recall archived detail with the ultracompress_recall tool)';
    const capped = trigger === 'manual' ? summary : capSummaryUtf8(summary, SUMMARY_MAX_BYTES);
    // The transcript line is cleared with the compacted conversation, so
    // the result also shows as a toast the person actually sees.
    const notice = formatCompactNotice(rc.stats);
    try {
      $.ui.log(notice);
    } catch {}
    try {
      $.ui.toast(notice, { timeoutMs: 8000 });
    } catch {}
    const result = {
      messages: [{ role: 'user', text: `${header}\n\n${capped}`, toolUses: [] }, ...messages.slice(cut)],
    };
    const stats = rc.stats && typeof rc.stats === 'object' ? rc.stats : {};
    if (typeof stats.tokens_before_est === 'number') result.tokensBefore = stats.tokens_before_est;
    if (typeof stats.tokens_after_est === 'number') result.tokensAfter = stats.tokens_after_est;
    return result;
  } catch (err) {
    return fail(String((err && err.message) || err));
  }
}

/**
 * This session's transcript path from its coordinates:
 * ${CLAUDE_CONFIG_DIR:-~/.claude}/projects/<cwd, every non [A-Za-z0-9] char as
 * '-'>/<session id>.jsonl — the same spelling Claude Code writes.
 */
export function transcriptPathFor(configDir, home, cwd, sessionId) {
  const base = typeof configDir === 'string' && configDir.trim() ? configDir : `${home}/.claude`;
  const encoded = String(cwd).replace(/[^A-Za-z0-9]/g, '-');
  return `${base}/projects/${encoded}/${sessionId}.jsonl`;
}

/**
 * What the recall tool searches: an explicit transcript path, or this
 * session's own file, stat-checked as a regular file. Only one session is ever
 * opened — never the archive.
 */
export async function resolveTranscript($, explicit) {
  if (explicit !== undefined) {
    if (typeof explicit !== 'string' || !explicit.trim()) throw new Error('sessionFile must be a non-empty path');
    if (explicit.length > 4096) throw new Error('sessionFile is too long');
    return explicit;
  }
  const [id, cwd] = await Promise.all([$.session.id(), $.session.cwd()]);
  if (!id || !cwd) throw new Error('No session file available; supply an explicit sessionFile');
  const [configDir, home] = await Promise.all([$.env.get('CLAUDE_CONFIG_DIR'), $.env.get('HOME')]);
  const hasConfigDir = typeof configDir === 'string' && Boolean(configDir.trim());
  if (!hasConfigDir && !home) throw new Error('No session file available; supply an explicit sessionFile');
  const path = transcriptPathFor(hasConfigDir ? configDir : undefined, home, cwd, id);
  const st = await $.fs.stat(path).catch(() => undefined);
  if (!st || st.kind !== 'file') {
    throw new Error(`this session's transcript is missing at ${path}; supply an explicit sessionFile`);
  }
  return path;
}

/**
 * Pure binary argv from validated params (Pi recallArgs): the binary reads the
 * Claude transcript natively, so no conversion and no --leaf — a Claude
 * transcript has no leaf API. Default scope all: lineage alone stops at the
 * newest compaction boundary in a real transcript.
 */
export function recallArgvFrom(params, transcript) {
  for (const key of Object.keys(params ?? {})) {
    if (!Object.hasOwn(RECALL_TOOL.inputSchema.properties, key)) throw new Error(`Unknown recall option: ${key}`);
  }
  const query = params?.query;
  if (typeof query !== 'string' || !query.trim() || Array.from(query).length > 512) {
    throw new Error('query must contain 1–512 characters');
  }
  if (typeof transcript !== 'string' || !transcript) {
    throw new Error('No session file available; supply an explicit sessionFile');
  }
  const scope = params?.scope === undefined ? 'all' : params.scope;
  if (scope !== 'lineage' && scope !== 'all') throw new Error('scope must be lineage or all (within one session)');
  const args = ['recall', '--session', transcript, '--format', 'claude', '--query', query, '--scope', scope];
  if (params?.regex !== undefined) {
    if (typeof params.regex !== 'boolean') throw new Error('regex must be boolean');
    if (params.regex) args.push('--regex');
  }
  for (const [key, flag] of [
    ['role', '--role'],
    ['toolName', '--tool-name'],
    ['afterEntry', '--after-entry'],
    ['beforeEntry', '--before-entry'],
  ]) {
    const value = params?.[key];
    if (value === undefined) continue;
    if (typeof value !== 'string' || !value || value.length > 512) throw new Error(`${key} must contain 1–512 characters`);
    const allowed = RECALL_STRING_PARAMS[key];
    if (allowed && !allowed.includes(value)) throw new Error(`Invalid ${key}`);
    args.push(flag, value);
  }
  for (const [key, flag] of [
    ['page', '--page'],
    ['perPage', '--per-page'],
    ['snippetBytes', '--snippet-bytes'],
    ['maxOutputBytes', '--max-output-bytes'],
  ]) {
    const value = params?.[key];
    if (value === undefined) continue;
    const [min, max] = RECALL_INT_BOUNDS[key];
    if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) {
      throw new Error(`${key} must be an integer from ${min} to ${max}`);
    }
    args.push(flag, String(value));
  }
  if (params?.snippetBytes === undefined) args.push('--snippet-bytes', '4000');
  return args;
}

/** Pi recallText: JSON only, within the byte budget, never truncated silently. */
export function boundedRecallText(text, budget = RECALL_MAX_OUTPUT_BYTES) {
  const bytes = new TextEncoder().encode(String(text ?? ''));
  if (!bytes.length || bytes.length > budget) {
    throw new Error('Recall bridge exceeded the output byte budget; request smaller excerpts');
  }
  return String(text);
}

/** tool.call hook serving mcp__ultracompress__ultracompress_recall via the binary itself. */
export async function handleRecallCall($, e) {
  try {
    const bin = await ultraCompressBin($);
    const st = await $.fs.stat(bin).catch(() => undefined);
    if (!st || st.kind !== 'file') {
      return { result: `UltraCompress recall failed: binary missing at ${bin}` };
    }
    // tool.call's input carries the engine's reserved keys beside the tool's
    // own arguments (ToolCallReserved + AgentLoop); only the arguments count.
    const { tool: _tool, tool_use_id: _id, consent: _consent, agentId: _agent, ...params } = e ?? {};
    const transcript = await resolveTranscript($, params.sessionFile);
    const argv = recallArgvFrom(params, transcript);
    const res = await $.process.run([bin, ...argv], {
      env: await bridgeEnv($),
      timeoutMs: COMPACT_TIMEOUT_MS,
    });
    if (res.exitCode !== 0) {
      return { result: `UltraCompress recall failed: ${excerpt(res.stderr, 300) || `exit ${res.exitCode}`}` };
    }
    return { result: boundedRecallText(res.stdout) };
  } catch (err) {
    return { result: `UltraCompress recall failed: ${String((err && err.message) || err)}` };
  }
}

/**
 * Register the UltraCompress function hooks:
 *  - session.compact: UltraCompress owns manual/auto/plugin compaction; precompute skips.
 *  - session.start:   registers the ultracompress_recall tool (listed by turn one).
 *  - tool.call:       serves mcp__ultracompress__ultracompress_recall by running the
 *                     binary's recall over this session's transcript directly.
 */
export function register(on, options) {
  on('session.compact', handleCompact);
  on('session.start', ($, e, next) =>
    $.tool
      .register({ name: RECALL_TOOL.name, description: RECALL_TOOL.description, inputSchema: RECALL_TOOL.inputSchema })
      .catch((err) => {
        try {
          $.ui.log(`ultracompress_recall not registered: ${String((err && err.message) || err)}`, { to: 'debug' });
        } catch {}
      })
      .then(() => next(e)),
  );
  on('tool.call', { tool: RECALL_TOOL.matcher }, handleRecallCall);
}
