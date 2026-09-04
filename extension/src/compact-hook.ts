/**
 * Compaction hook: pure functions bridging Pi's session_before_compact event
 * to the UltraCompress binary contract. Everything here is vitest-testable without a
 * running Pi.
 */

export interface BranchEntry {
  type?: string;
  id?: string;
  message?: { role?: string; [k: string]: unknown };
  [k: string]: unknown;
}

export interface CompactEventLike {
  preparation?: {
    tokensBefore?: number;
    previousSummary?: string;
    [k: string]: unknown;
  };
  branchEntries?: BranchEntry[];
  customInstructions?: string;
  reason?: string;
  willRetry?: boolean;
}

export interface CompactStdin {
  entries: BranchEntry[];
  tokensBefore?: number;
  previousSummary?: string;
  policy: string;
  keepUserTurns: number | null;
  smartKeepTail: boolean;
  vision: string;
  modelVision: boolean | null;
  ucBin: string;
  ucEnabled: boolean;
  ucMinChars: number;
  snapMinChars: number;
}

export interface UltraCompressCompactStats {
  tokens_before_est: number;
  tokens_after_est: number;
  savings_pct: number;
  summarized_messages: number;
  kept_messages: number;
  keep_user_turns_resolved: number;
  smart_keep_adjusted: boolean;
  uc_blocks: number;
  snap_blocks: number;
  chars_per_token: number;
  calibrated: boolean;
}

export interface UltraCompressCompactResult {
  summary: string;
  first_kept_entry_id: string;
  details: Record<string, unknown>;
  stats: UltraCompressCompactStats;
  uc_status: { available: boolean; version?: string; reason?: string };
}

/** /ultracompress args: `keep:N [prompt…]`, `policy:<p>`, bare prompt text. */
export function parseUltraCompressArgs(raw: string | undefined): { keep: number | null; policy: string | null; prompt: string } {
  const keepMatch = /(?:^|\s)keep:(\d+)(?=\s|$)/.exec(raw ?? "");
  const policyMatch = /(?:^|\s)policy:(auto|vcc|snap|uc)(?=\s|$)/.exec(raw ?? "");
  let prompt = (raw ?? "")
    .replace(/(?:^|\s)keep:\d+(?=\s|$)/, "")
    .replace(/(?:^|\s)policy:(auto|vcc|snap|uc)(?=\s|$)/, "")
    .trim();
  if ((prompt.startsWith('"') && prompt.endsWith('"')) || (prompt.startsWith("'") && prompt.endsWith("'"))) {
    prompt = prompt.slice(1, -1);
  }
  return {
    keep: keepMatch ? Math.max(0, parseInt(keepMatch[1], 10)) : null,
    policy: policyMatch ? policyMatch[1] : null,
    prompt,
  };
}

/** Build the UltraCompress compact stdin payload from the event. */
export function buildCompactStdin(event: CompactEventLike, opts: {
  policy: string;
  keepUserTurns: number | null;
  smartKeepTail: boolean;
  modelVision: boolean | null;
  ucBin: string;
  ucEnabled: boolean;
  ucMinChars: number;
  snapMinChars: number;
}): CompactStdin {
  const entries = (event.branchEntries ?? []).filter(
    (e) => e.type === "message" || e.type === "compaction",
  );
  const stdin: CompactStdin = {
    entries,
    policy: opts.policy,
    keepUserTurns: opts.keepUserTurns,
    smartKeepTail: opts.smartKeepTail,
    vision: opts.modelVision ? "on" : "auto",
    modelVision: opts.modelVision,
    ucBin: opts.ucBin,
    ucEnabled: opts.ucEnabled,
    ucMinChars: opts.ucMinChars,
    snapMinChars: opts.snapMinChars,
  };
  if (typeof event.preparation?.tokensBefore === "number") stdin.tokensBefore = event.preparation.tokensBefore;
  if (typeof event.preparation?.previousSummary === "string") stdin.previousSummary = event.preparation.previousSummary;
  return stdin;
}

/** Map UltraCompress result → pi compaction return; null when UltraCompress can't own this compact. */
export function toCompactionResult(rc: UltraCompressCompactResult, tokensBefore?: number): {
  summary: string;
  firstKeptEntryId: string;
  tokensBefore?: number;
  details: Record<string, unknown>;
} | null {
  if (!rc.summary || typeof rc.summary !== "string") return null;
  return {
    summary: rc.summary,
    firstKeptEntryId: rc.first_kept_entry_id ?? "",
    ...(typeof tokensBefore === "number" ? { tokensBefore } : {}),
    details: {
      ...rc.details,
      compactor: "ultracompress",
      ultracompressStats: rc.stats,
    },
  };
}

export function formatStatsLine(rc: UltraCompressCompactResult): string {
  const s = rc.stats;
  const k = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
  const parts = [
    `ultracompress: ${k(s.tokens_before_est)} → ${k(s.tokens_after_est)} tok (-${s.savings_pct.toFixed(0)}%)`,
    `summarized ${s.summarized_messages}, kept ${s.kept_messages}`,
  ];
  if (s.smart_keep_adjusted) parts.push(`smart-keep → ${s.keep_user_turns_resolved}`);
  if (s.uc_blocks > 0) parts.push(`uc ×${s.uc_blocks}`);
  return parts.join(" · ");
}
