import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { runUltraCompress } from "./src/bridge";
import { UcReferences } from "./src/references";
import { recallArgs, recallProperties, recallText, parseRecallCommand } from "./src/recall";
import { loadSettings, resolveUltraCompressBin, type UltraCompressSettings } from "./src/settings";
import { listSnaps, writeSnapEntries } from "./src/snapshot";
import {
  applyTransforms,
  cacheKey,
  capCache,
  collectCandidates,
  nextUserIndex,
  type AgentLikeMessage,
  type UltraCompressOp,
  type TransformResponse,
} from "./src/transforms";
import {
  buildCompactStdin,
  formatStatsLine,
  parseUltraCompressArgs,
  toCompactionResult,
  type CompactEventLike,
  type UltraCompressCompactResult,
} from "./src/compact-hook";

/**
 * UltraCompress — content-aware compaction for Pi.
 *
 * Compaction:  deterministic VCC brief via the UltraCompress binary (no LLM call),
 *              UC packets inline for JSON payloads, smart keep-tail,
 *              token-budget tail rescue, pre-compaction snapshots.
 * Live path:   oversized tool results become UC packets or snap PNG frames
 *              (fixed vision cost) before each LLM call — memoized by hash.
 * Recall:      ultracompress_recall searches the raw session JSONL, so compacted-away
 *              history stays reachable. Lossless.
 *
 * Failure posture: every UltraCompress call is best-effort. If the binary is missing or
 * errors, compaction falls through to Pi core and the live path degrades to
 * stock text. UltraCompress never bricks a session.
 */

const AUTO_CONTINUE_CUSTOM_TYPE = "ultracompress-auto-continue";

export default function ultraCompressExtension(pi: ExtensionAPI): void {
  const settings: UltraCompressSettings = loadSettings();
  const ultracompressBin = resolveUltraCompressBin(settings);
  const transformCache = new Map<string, { op: UltraCompressOp; blocks: Array<Record<string, unknown>> }>();
  const references = new UcReferences();
  pi.on("session_start", () => {
    references.clear();
    transformCache.clear();
  });
  let lastCalibratedCpt: number | undefined;
  let visionKnown: boolean | null = null;

  const dbg = (data: Record<string, unknown>) => {
    if (!settings.debug) return;
    try {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      const { writeFileSync } = require("node:fs") as typeof import("node:fs");
      writeFileSync("/tmp/ultracompress-debug.json", JSON.stringify(data, null, 2));
    } catch {}
  };

  const keyFor = (text: string) =>
    cacheKey(
      { p: settings.policy, v: visionKnown, s: settings.snap.minChars, u: settings.uc.minChars, cpt: lastCalibratedCpt },
      text,
    );

  // Remove our invisible-continue marker from LLM payloads (matched by type only).
  pi.on("context", (event) => {
    const messages = event.messages.filter(
      (m: { role?: string; customType?: string }) =>
        !(m.role === "custom" && m.customType === AUTO_CONTINUE_CUSTOM_TYPE),
    );
    if (messages.length !== event.messages.length) return { messages };
    return undefined;
  });

  // ── Live transforms ────────────────────────────────────────────────────
  pi.on("context", async (event, ctx) => {
    if (!settings.snap.enabled && !settings.uc.enabled) return undefined;
    if (visionKnown === null) {
      try {
        const model = ctx?.model as { input?: string[]; provider?: string } | undefined;
        const registryVision = Array.isArray(model?.input) ? model!.input.includes("image") : false;
        const providerOk = settings.snap.providers.includes(String(model?.provider ?? ""));
        visionKnown = registryVision && providerOk;
      } catch {
        visionKnown = null;
      }
    }
    const minChars = Math.min(settings.uc.minChars, settings.snap.minChars);
    const messages = event.messages as unknown as AgentLikeMessage[];
    // Reuse keys only within this request: repeated text and the apply pass need
    // no second serialization/hash. Keep candidate occurrences (and their order)
    // intact, and recompute with current settings/calibration on the next request.
    const requestKeys = new Map<string, string>();
    const requestKeyFor = (text: string): string => {
      let key = requestKeys.get(text);
      if (key === undefined) {
        key = keyFor(text);
        requestKeys.set(text, key);
      }
      return key;
    };
    const candidates = collectCandidates(messages, minChars, requestKeyFor);
    const fresh = candidates.filter((c) => !transformCache.has(c.key));
    if (fresh.length > 0) {
      // Batch-compute transforms for unseen blocks in one UltraCompress call. Synthetic
      // minimal messages keep indices stable: one candidate per message.
      const payload = {
        messages: fresh.map((c) => ({
          type: "message",
          id: `rc${c.messageIndex}`,
          message: { role: "toolResult", content: [{ type: "text", text: c.text }] },
        })),
        policy: settings.policy,
        vision: visionKnown ? "on" : "auto",
        modelVision: visionKnown,
        ucBin: settings.uc.bin,
        ucEnabled: settings.uc.enabled,
        snapMinChars: settings.snap.minChars,
        ucMinChars: settings.uc.minChars,
        ...(lastCalibratedCpt ? { charsPerToken: lastCalibratedCpt } : {}),
      };
      const res = await runUltraCompress<TransformResponse>(
        ultracompressBin,
        [
          "transform",
          "--policy",
          String(payload.policy),
          "--vision",
          !settings.snap.enabled ? "off" : (payload.vision as string),
        ],
        payload,
      );
      if (res.ok && res.data?.ops) {
        for (const op of res.data.ops) {
          const c = fresh[op.message_index];
          if (!c) continue;
          const blocks =
            op.op === "uc"
              ? [{ type: "text", text: `${op.stub}\n\n${op.packet}` }]
              : null;
          if (blocks) transformCache.set(c.key, { op, blocks });
          else if (op.op === "snap") {
            // Snap replacement is assembled at apply time (frames may travel
            // to the next user message); store the op with edge text blocks.
            transformCache.set(c.key, {
              op,
              blocks: [
                {
                  type: "text",
                  text: `${op.head}\n[ultracompress: ${op.frames.length} image frame(s) hold the archived middle — ${op.frames
                    .map((f) => f.id)
                    .join(", ")}]${op.tail}`,
                },
              ],
            });
          }
        }
        capCache(transformCache);
      } else if (res.error) {
        dbg({ transformError: res.error });
      }
    }

    if (transformCache.size === 0) return undefined;

    // Rehydrate references from original context even after bounded-cache eviction.
    for (const candidate of candidates) {
      const entry = transformCache.get(candidate.key);
      if (entry?.op.op === "uc") {
        const reference = references.put(candidate.text);
        if (reference) entry.op.reference = reference;
        else transformCache.delete(candidate.key); // too large: preserve stock text
      }
    }

    // A single request may exceed the reference budget. Never emit an evicted handle.
    for (const candidate of candidates) {
      const entry = transformCache.get(candidate.key);
      if (entry?.op.op === "uc" && entry.op.reference && references.get(entry.op.reference) === undefined) {
        transformCache.delete(candidate.key);
      }
    }

    const keyFn = (m: AgentLikeMessage, bi: number): string | undefined => {
      if (m.role !== "toolResult") return undefined;
      const content = m.content;
      if (typeof content === "string") return content.length >= minChars ? requestKeyFor(content) : undefined;
      const block = content?.[bi] as { text?: unknown } | undefined;
      if (!block || typeof block.text !== "string") return undefined;
      return block.text.length >= minChars ? requestKeyFor(block.text) : undefined;
    };

    const result = applyTransforms(messages, transformCache, keyFn, settings.snap.placement);
    if (result.ucApplied + result.snapApplied === 0) return undefined;
    return { messages: result.messages as unknown as typeof event.messages };
  });

  // ── Compaction ─────────────────────────────────────────────────────────
  // Pi exposes this hook at runtime; older extension type declarations omit it.
  const onCompaction = pi.on as unknown as (
    event: "session_before_compact",
    handler: (event: unknown, ctx: any) => Promise<unknown>,
  ) => void;
  onCompaction("session_before_compact", async (event, ctx) => {
    const ev = event as unknown as CompactEventLike;
    const custom = ev.customInstructions?.trim() ?? "";
    const isExplicitUltraCompress = custom === "/ultracompress" || custom.startsWith("/ultracompress ");
    if (!isExplicitUltraCompress && !settings.overrideDefaultCompaction) return undefined;

    const args = parseUltraCompressArgs(
      isExplicitUltraCompress ? custom.slice("/ultracompress".length) : custom,
    );
    const modelVision = (() => {
      try {
        const model = ctx?.model as { input?: string[]; provider?: string } | undefined;
        const registryVision = Array.isArray(model?.input) ? model!.input.includes("image") : false;
        return registryVision && settings.snap.providers.includes(String(model?.provider ?? ""));
      } catch {
        return null;
      }
    })();

    // Safety snapshot first — never blocks compaction.
    if (settings.snapshot.enabled) {
      try {
        const entries = ctx.sessionManager.getEntries() ?? [];
        writeSnapEntries(process.cwd(), entries, ev.reason ?? "unknown", Date.now());
      } catch (error) {
        console.error("[ultracompress] snapshot failed:", error);
      }
    }

    const stdin = buildCompactStdin(ev, {
      policy: args.policy ?? settings.policy,
      keepUserTurns: args.keep ?? settings.keepUserTurns,
      smartKeepTail: settings.smartKeepTail,
      modelVision,
      ucBin: settings.uc.bin,
      ucEnabled: settings.uc.enabled,
      ucMinChars: settings.uc.minChars,
      snapMinChars: settings.snap.minChars,
    });

    const rcArgs = ["compact", "--policy", stdin.policy, "--vision", stdin.vision as string];
    const res = await runUltraCompress<UltraCompressCompactResult>(ultracompressBin, rcArgs, stdin, 30_000);
    if (!res.ok || !res.data) {
      // Never brick the session: fall through to Pi core compaction.
      dbg({ compactError: res.error, reason: ev.reason });
      try {
        ctx?.ui?.notify?.(`ultracompress: UltraCompress failed, falling back to core (${res.error?.slice(0, 80)})`, "warning");
      } catch {}
      return undefined;
    }

    const rc = res.data;
    if (typeof rc.stats?.chars_per_token === "number" && rc.stats.calibrated) {
      lastCalibratedCpt = rc.stats.chars_per_token;
    }

    const mapped = toCompactionResult(rc, ev.preparation?.tokensBefore);
    if (!mapped) return undefined;

    try {
      ctx?.ui?.notify?.(formatStatsLine(rc), "info");
    } catch {}
    dbg({ compact: { reason: ev.reason, stats: rc.stats, summaryPreview: rc.summary.slice(0, 400) } });
    return { compaction: mapped };
  });

  // ── Commands ───────────────────────────────────────────────────────────
  pi.registerCommand("ultracompress", {
    description: "Compact now with UltraCompress (keep:N policy:auto|vcc|snap|uc, optional follow-up prompt)",
    handler: async (args, ctx) => {
      const custom = args?.trim() ? `/ultracompress ${args.trim()}` : "/ultracompress";
      try {
        await ctx.compact({
          customInstructions: custom,
          onComplete: () => ctx.ui.notify("ultracompress: compaction complete", "info"),
          onError: (error: Error) => ctx.ui.notify(`ultracompress failed: ${error.message}`, "error"),
        });
      } catch (error) {
        ctx.ui.notify(`ultracompress failed: ${String(error)}`, "error");
      }
    },
  });

  pi.registerCommand("ultracompress-recall", {
    description: "Search this session's raw history (compacted turns included)",
    handler: async (args, ctx) => {
      try {
        const params = parseRecallCommand(args?.trim() ?? "");
        const argv = recallArgs(params, ctx.sessionManager);
        const res = await runUltraCompress(ultracompressBin, argv, null);
        if (!res.ok || !res.data) throw new Error(res.error ?? "No recall result");
        await ctx.ui.notify(recallText(res.data, Number(params.maxOutputBytes ?? 12000)), "info");
      } catch (error) {
        await ctx.ui.notify(`UltraCompress recall: ${String(error)}`, "error");
      }
    },
  });

  pi.registerCommand("ultracompress-stats", {
    description: "UltraCompress status and settings",
    handler: async (_args, ctx) => {
      const lines = [
        `ultracompress ${"0.2.1"}`,
        `UltraCompress binary: ${ultracompressBin}`,
        `policy: ${settings.policy} · override: ${settings.overrideDefaultCompaction} · smart-keep: ${settings.smartKeepTail}`,
        `uc: ${settings.uc.enabled ? "on" : "off"} (${settings.uc.bin}) · snap: ${settings.snap.enabled ? "on" : "off"} (placement: ${settings.snap.placement})`,
        `transforms cached: ${transformCache.size}`,
        `snapshots: ${listSnaps(process.cwd()).length} in .steak-pi/snaps`,
      ];
      await ctx.ui.notify(lines.join("\n"), "info");
    },
  });

  pi.registerCommand("snaps", {
    description: "List pre-compaction snapshots",
    handler: async (_args, ctx) => {
      const snaps = listSnaps(process.cwd());
      const text = snaps.length
        ? snaps.map((s) => `${new Date(s.createdAt).toISOString()} · ${s.reason} · ${s.entries} entries · ${s.compactor}`).join("\n")
        : "No snapshots yet.";
      await ctx.ui.notify(text, "info");
    },
  });

  // ── Tools ──────────────────────────────────────────────────────────────
  pi.registerTool({
    name: "ultracompress_recall",
    label: "UltraCompress Recall",
    description:
      "Search raw pre-compaction history before claiming context is lost. Defaults to this session's current " +
      "lineage only. scope:all includes sibling branches of that same session, never other sessions. " +
      "Supply sessionFile explicitly for another session. Narrow by role, tool, entry range and bounded excerpts. " +
      "Results identify the searched session, scope and message count; no automatic widening.",
    promptSnippet:
      "ultracompress_recall: current session/current lineage by default; all = sibling branches only; another session requires explicit sessionFile. Use narrow filters and small pages.",
    parameters: {
      type: "object",
      properties: recallProperties,
      required: ["query"],
      additionalProperties: false,
    },
    async execute(_id, params, _signal, _onUpdate, ctx) {
      try {
        const argv = recallArgs(params, ctx.sessionManager);
        const res = await runUltraCompress(ultracompressBin, argv, null);
        if (!res.ok || !res.data) throw new Error(res.error ?? "No recall result");
        return { content: [{ type: "text", text: recallText(res.data, Number(params.maxOutputBytes ?? 12000)) }], details: {} };
      } catch (error) {
        return { content: [{ type: "text", text: `UltraCompress recall failed: ${String(error)}` }], details: {}, isError: true };
      }
    },
  });

  pi.registerTool({
    name: "ultracompress_uc",
    label: "UC Decode",
    description:
      "Retrieve exact original tool output using the uc:<hash> reference printed in its archive marker. " +
      "Pass that reference as packet; never reconstruct or abbreviate an encoded payload. " +
      "Legacy complete @UC1 packets are also accepted and decode to JSON. " +
      "Only legacy packets explicitly marked as envelopes wrap original text in the JSON key t.",
    promptSnippet: "ultracompress_uc: retrieve original tool output by its uc:<hash> reference; legacy complete @UC1 packets also supported.",
    parameters: {
      type: "object",
      properties: {
        packet: { type: "string", description: "The uc:<hash> reference from the archive marker (preferred), or a complete legacy @UC1 packet." },
      },
      required: ["packet"],
    },
    async execute(_id, params) {
      const packet = String(params.packet ?? "");
      if (!packet) return { content: [{ type: "text", text: "No packet provided." }], details: {} };
      if (packet.startsWith("uc:")) {
        const text = references.get(packet.trim());
        return { content: [{ type: "text", text: text ??
          "UC reference is unavailable in this session. Use ultracompress_recall or re-read the original source; do not invent a packet or retry this missing reference." }], details: {} };
      }
      const res = await runUltraCompress<{ decoded?: string; error?: string }>(
        ultracompressBin, ["uc", "decode"], { packet, ucBin: settings.uc.bin },
      );
      if (!res.ok || !res.data) {
        return { content: [{ type: "text", text: `UltraCompress UC decode failed: ${res.error}. Use the uc:<hash> reference when available, or retrieve the original with ultracompress_recall. Do not retry an unchanged incomplete packet.` }], details: {} };
      }
      if (res.data.error) {
        return { content: [{ type: "text", text: `decode error: ${res.data.error}. Use the uc:<hash> reference when available, or retrieve the original with ultracompress_recall. This error alone does not identify the cause; do not retry an unchanged incomplete packet.` }], details: {} };
      }
      return { content: [{ type: "text", text: res.data.decoded ?? "(empty)" }], details: {} };
    },
  });
}
