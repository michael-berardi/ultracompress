import { createHash } from "node:crypto";

/**
 * Live-context transforms: oversized tool results become UC packets (JSON)
 * or snap frames (bulky text) before the LLM sees them. The raw session
 * stays untouched — transforms are a per-request lens, recomputed from the
 * deep-copied context each call, memoized by content hash.
 */

export interface UcOp {
  op: "uc";
  message_index: number;
  block_index: number;
  stub: string;
  packet: string;
  tokens_before: number;
  tokens_after: number;
}

export interface FrameOut {
  id: string;
  width: number;
  height: number;
  pngBase64: string;
}

export interface SnapOp {
  op: "snap";
  message_index: number;
  block_index: number;
  head: string;
  tail: string;
  frames: FrameOut[];
  tokens_before: number;
  tokens_after: number;
}

export type UltraCompressOp = UcOp | SnapOp;

export interface TransformResponse {
  ops: UltraCompressOp[];
  stats: {
    blocks_scanned: number;
    uc_ops: number;
    snap_ops: number;
    tokens_before: number;
    tokens_after: number;
    savings_pct: number;
    uc_available: boolean;
  };
  ucStatus?: { available: boolean; version?: string };
}

/** Any content shape: string or block array. */
export type Content = string | Array<Record<string, unknown>>;

export interface AgentLikeMessage {
  role: string;
  content: Content;
  [k: string]: unknown;
}

export function sha256(text: string): string {
  return createHash("sha256").update(text).digest("hex").slice(0, 32);
}

export function cacheKey(parts: Record<string, unknown>, text: string): string {
  return sha256(JSON.stringify(parts) + "\u0000" + text);
}

/** Flatten content blocks to text blocks with their indices. */
export function textBlocks(m: AgentLikeMessage): Array<{ index: number; text: string }> {
  if (typeof m.content === "string") return m.content ? [{ index: 0, text: m.content }] : [];
  return (m.content ?? [])
    .map((b, index) => ({ index, text: typeof b?.text === "string" ? (b.text as string) : null }))
    .filter((b): b is { index: number; text: string } => b.text !== null);
}

export interface Candidate {
  messageIndex: number;
  blockIndex: number;
  text: string;
  key: string;
}

/** Find transform candidates: toolResult text blocks above the char floor. */
export function collectCandidates(messages: AgentLikeMessage[], minChars: number, keyFn: (text: string) => string): Candidate[] {
  const out: Candidate[] = [];
  messages.forEach((m, messageIndex) => {
    if (m.role !== "toolResult") return;
    for (const { index: blockIndex, text } of textBlocks(m)) {
      if (text.length >= minChars) {
        out.push({ messageIndex, blockIndex, text, key: keyFn(text) });
      }
    }
  });
  return out;
}

/** Blocks that replace a tool-result text block for a UC op. */
export function ucReplacement(op: UcOp): Array<Record<string, unknown>> {
  return [
    {
      type: "text",
      text: `${op.stub}\n\n${op.packet}`,
    },
  ];
}

/** Text edge blocks kept in the tool result for a snap op (frames travel separately). */
export function snapTextReplacement(op: SnapOp): Array<Record<string, unknown>> {
  const marker =
    op.frames.length > 0
      ? `\n[ultracompress: ${op.frames.length} image frame(s) hold the archived middle — ${
          op.frames.map((f) => f.id).join(", ")
        }]`
      : "";
  return [
    {
      type: "text",
      text: `${op.head}${marker}${op.tail}`,
    },
  ];
}

/** Image blocks for snap frames, with a caption. */
export function snapFrameBlocks(op: SnapOp): Array<Record<string, unknown>> {
  const blocks: Array<Record<string, unknown>> = [];
  if (op.frames.length === 0) return blocks;
  blocks.push({
    type: "text",
    text: `[ultracompress: archived tool output follows as image frame(s) — verbatim, read in order]`,
  });
  for (const f of op.frames) {
    blocks.push({ type: "image", data: f.pngBase64, mimeType: "image/png" });
  }
  return blocks;
}

export interface ApplyResult {
  messages: AgentLikeMessage[];
  ucApplied: number;
  snapApplied: number;
}

/**
 * Apply cached replacements to a deep-copied message list.
 * Snap frames go to the next user message ("nextUser" placement, safe across
 * providers) or replace the block inline ("inline").
 */
export function applyTransforms(
  messages: AgentLikeMessage[],
  replacements: Map<string, { op: UltraCompressOp; blocks: Array<Record<string, unknown>> }>,
  keys: (m: AgentLikeMessage, bi: number) => string | undefined,
  placement: "nextUser" | "inline",
): ApplyResult {
  let ucApplied = 0;
  let snapApplied = 0;
  const pendingFrames: Array<{ userIndex: number; blocks: Array<Record<string, unknown>> }> = [];

  messages.forEach((m, mi) => {
    if (m.role !== "toolResult" || typeof m.content === "string") return;
    const content = m.content as Array<Record<string, unknown>>;
    for (let bi = 0; bi < content.length; bi++) {
      const key = keys(m, bi);
      if (!key) continue;
      const entry = replacements.get(key);
      if (!entry) continue;
      if (entry.op.op === "uc") {
        const replacement = ucReplacement(entry.op as UcOp);
        content.splice(bi, 1, ...replacement);
        bi += replacement.length - 1; // don't rescan inserted blocks
        ucApplied++;
      } else {
        const op = entry.op as SnapOp;
        if (placement === "inline") {
          const replacement = [...snapTextReplacement(op), ...snapFrameBlocks(op)];
          content.splice(bi, 1, ...replacement);
          bi += replacement.length - 1; // don't rescan inserted blocks
        } else {
          const replacement = snapTextReplacement(op);
          content.splice(bi, 1, ...replacement);
          bi += replacement.length - 1;
          const userIndex = nextUserIndex(messages, mi);
          pendingFrames.push({ userIndex, blocks: snapFrameBlocks(op) });
        }
        snapApplied++;
      }
    }
  });

  if (placement === "nextUser" && pendingFrames.length > 0) {
    // Group frame blocks per target user message.
    const byUser = new Map<number, Array<Record<string, unknown>>>();
    for (const pf of pendingFrames) {
      const list = byUser.get(pf.userIndex) ?? [];
      list.push(...pf.blocks);
      byUser.set(pf.userIndex, list);
    }
    for (const [userIndex, blocks] of byUser) {
      const target = messages[userIndex];
      if (!target) continue;
      if (typeof target.content === "string") {
        target.content = [{ type: "text", text: target.content }, ...blocks];
      } else if (Array.isArray(target.content)) {
        target.content = [...target.content, ...blocks];
      }
    }
  }

  return { messages, ucApplied, snapApplied };
}

export function nextUserIndex(messages: AgentLikeMessage[], from: number): number {
  for (let i = from + 1; i < messages.length; i++) {
    if (messages[i].role === "user") return i;
  }
  for (let i = from - 1; i >= 0; i--) {
    if (messages[i].role === "user") return i;
  }
  return -1;
}

/** LRU-ish cap for the memo cache. */
export function capCache(cache: Map<string, unknown>, max = 600): void {
  if (cache.size <= max) return;
  const drop = cache.size - max;
  let dropped = 0;
  for (const k of cache.keys()) {
    cache.delete(k);
    if (++dropped >= drop) break;
  }
}
