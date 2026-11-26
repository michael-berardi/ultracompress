import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { writeFileSync } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { runUltraCompress } from "./src/bridge";
import { recallArgs, recallProperties, recallText, parseRecallCommand } from "./src/recall";
import {
  isUltraCompressBinAvailable,
  loadSettings,
  packageVersion,
  resolveUltraCompressBin,
  type UltraCompressSettings,
} from "./src/settings";
import { TextKeyCache } from "./src/text-key-cache";
import { countSnaps, listSnaps, writeSnapEntries } from "./src/snapshot";
import {
  applyTransforms,
  cacheKey,
  capCache,
  collectCandidates,
  snapTextReplacement,
  type AgentLikeMessage,
  type UltraCompressOp,
  type TransformResponse,
} from "./src/transforms";
import {
  buildCompactStdin,
  capSummary,
  formatStatsLine,
  parseUltraCompressArgs,
  toCompactionResult,
  type CompactEventLike,
  type UltraCompressCompactResult,
} from "./src/compact-hook";
import { NOTICE_RECHECK_MS, UPDATE_STATUS_KEY, footerText, runCli, updateArgs } from "./src/update-notice";

/**
 * UltraCompress — content-aware compaction for Pi.
 *
 * Compaction:  deterministic VCC brief via the UltraCompress binary (no LLM call),
 *              smart keep-tail,
 *              token-budget tail rescue, pre-compaction snapshots.
 * Live path:   oversized tool results become snap PNG frames
 *              (fixed vision cost) before each LLM call — memoized by hash.
 * Recall:      ultracompress_recall searches the raw session JSONL, so compacted-away
 *              history stays reachable. Lossless.
 *
 * Failure posture: every UltraCompress call is best-effort. If the binary is missing or
 * errors, compaction falls through to Pi core and the live path degrades to
 * stock text. UltraCompress never bricks a session.
 */

/** Retries allowed for one block before a failing transform is left alone for the session. */
const MAX_TRANSFORM_FAILURES = 2;

export default function ultraCompressExtension(pi: ExtensionAPI): void {
  const settings: UltraCompressSettings = loadSettings();
  const ultracompressBin = resolveUltraCompressBin(settings);
  // Resolved once: without the binary, live transforms would spawn a failing
  // process on every request that carries a large tool result.
  const binAvailable = isUltraCompressBinAvailable(ultracompressBin);
  const transformCache = new Map<string, { op: UltraCompressOp; blocks: Array<Record<string, unknown>> }>();
  const noGainCache = new Map<string, true>();
  const transformFailures = new Map<string, number>();
  const textKeys = new TextKeyCache();
  let lastCalibratedCpt: number | undefined;
  let visionKnown: boolean | null = null;
  let lastNoticeAt = 0;

  // Footer notice in interactive sessions only; never in compaction output,
  // tool results, or print/JSON/RPC modes. The CLI decides whether one is due.
  const showUpdateNotice = (ctx: ExtensionContext) => {
    if (!settings.checkForUpdates || ctx.mode !== "tui" || !binAvailable) return;
    if (Date.now() - lastNoticeAt < NOTICE_RECHECK_MS) return;
    lastNoticeAt = Date.now();
    void runCli(ultracompressBin, ["update", "--notice"]).then((result) => {
      const text = footerText(result.stdout);
      if (!text) return;
      try {
        ctx.ui.setStatus(UPDATE_STATUS_KEY, ctx.ui.theme.fg("accent", text));
      } catch {
        // A host without a footer keeps the session working.
      }
    });
  };

  pi.on("session_start", (_event, ctx) => {
    transformCache.clear();
    noGainCache.clear();
    transformFailures.clear();
    textKeys.clear();
    // Calibration and vision describe one model in one session; never carry
    // a previous session's chars-per-token into the next one.
    lastCalibratedCpt = undefined;
    visionKnown = null;
    showUpdateNotice(ctx);
  });
  pi.on("turn_end", (_event, ctx) => {
    showUpdateNotice(ctx);
  });

  let debugWriteFailed = false;
  const dbg = (data: Record<string, unknown>) => {
    if (!settings.debug) return;
    try {
      writeFileSync(path.join(os.tmpdir(), "ultracompress-debug.json"), JSON.stringify(data, null, 2));
    } catch (error) {
      if (debugWriteFailed) return;
      debugWriteFailed = true;
      console.error("[ultracompress] debug write failed:", error);
    }
  };

  /** Whether this request's model can receive inline base64 frames. */
  const detectVision = (ctx: any): boolean | null => {
    try {
      const model = ctx?.model as { input?: string[]; provider?: string } | undefined;
      const registryVision = Array.isArray(model?.input) ? model!.input.includes("image") : false;
      const providerOk = settings.snap.providers.includes(String(model?.provider ?? ""));
      return registryVision && providerOk;
    } catch {
      return null;
    }
  };

  // ── Live transforms ────────────────────────────────────────────────────
  pi.on("context", async (event, ctx) => {
    if (!binAvailable || !settings.snap.enabled) return undefined;
    // Re-evaluate on model switches; a session-start vision latch can send
    // cached image transforms to a later text-only model.
    visionKnown = detectVision(ctx);
    // Without an image-capable model, or when the policy is text-only VCC,
    // every scanned block would come back unchanged — do not spawn at all.
    if (visionKnown !== true || settings.policy === "vcc") return undefined;
    const minChars = settings.snap.minChars;
    const messages = event.messages as unknown as AgentLikeMessage[];
    // Context objects are deep-copied by Pi: exact strings, not message IDs,
    // are the safe reuse boundary. Bound retained text and invalidate on settings.
    const keyParts = {
      p: settings.policy, v: visionKnown, s: settings.snap.minChars,
      snap: settings.snap.enabled,
      imageTokens: settings.snap.imageTokensPerFrame, cpt: lastCalibratedCpt,
    };
    textKeys.setScope(JSON.stringify(keyParts));
    const requestKeys = new Map<string, string>();
    const requestKeyFor = (text: string): string => {
      let key = requestKeys.get(text);
      if (key === undefined) {
        key = textKeys.get(text, () => cacheKey(keyParts, text));
        requestKeys.set(text, key);
      }
      return key;
    };
    const candidates = collectCandidates(messages, minChars, requestKeyFor);
    const unseen = new Set<string>();
    const fresh = candidates.filter((c) => {
      if (transformCache.has(c.key) || noGainCache.has(c.key) || unseen.has(c.key)) return false;
      unseen.add(c.key);
      return true;
    });
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
        snapMinChars: settings.snap.minChars,
        imageTokensPerFrame: settings.snap.imageTokensPerFrame,
        ...(lastCalibratedCpt ? { charsPerToken: lastCalibratedCpt } : {}),
      };
      const res = await runUltraCompress<TransformResponse>(
        ultracompressBin,
        [
          "transform",
          "--policy",
          String(payload.policy),
          "--vision",
          payload.vision as string,
        ],
        payload,
      );
      if (res.ok && res.data?.ops) {
        for (const op of res.data.ops) {
          const c = fresh[op.message_index];
          if (!c) continue;
          if (op.op === "snap") {
            // Snap replacement is assembled at apply time (frames may travel
            // to the next user message); store the op with its edge text block.
            transformCache.set(c.key, { op, blocks: snapTextReplacement(op) });
          }
        }
        // Explicit no-gain decisions from the core, when it reports them.
        for (const position of res.data.no_gain ?? []) {
          const c = fresh[position.message_index];
          if (c && position.block_index === 0 && !transformCache.has(c.key)) noGainCache.set(c.key, true);
        }
        // A successful response is also a decision for every block it left
        // alone: those stay text. Recording them keeps a non-snap block
        // (text-only model, VCC routing, older binary without `no_gain`) from
        // being re-sent to the CLI on every single request.
        for (const c of fresh) {
          if (!transformCache.has(c.key)) noGainCache.set(c.key, true);
        }
        transformFailures.clear();
        capCache(transformCache);
        capCache(noGainCache);
      } else if (res.error) {
        dbg({ transformError: res.error });
        // Repeated failures stop being retried for the rest of the session.
        for (const c of fresh) {
          const failures = (transformFailures.get(c.key) ?? 0) + 1;
          transformFailures.set(c.key, failures);
          if (failures >= MAX_TRANSFORM_FAILURES) noGainCache.set(c.key, true);
        }
        capCache(noGainCache);
      }
    }

    if (transformCache.size === 0) return undefined;

    const keyFn = (m: AgentLikeMessage, bi: number): string | undefined => {
      if (m.role !== "toolResult") return undefined;
      const content = m.content;
      if (typeof content === "string") return content.length >= minChars ? requestKeyFor(content) : undefined;
      const block = content?.[bi] as { text?: unknown } | undefined;
      if (!block || typeof block.text !== "string") return undefined;
      return block.text.length >= minChars ? requestKeyFor(block.text) : undefined;
    };

    const result = applyTransforms(messages, transformCache, keyFn, settings.snap.placement);
    if (result.snapApplied === 0) return undefined;
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
    // The brief is deterministic: free-text instructions cannot be honoured,
    // so say so once instead of dropping them silently.
    if (args.prompt) {
      try {
        ctx?.ui?.notify?.(
          "ultracompress: the deterministic brief does not use free-text instructions; it keeps the session's own structure",
          "warning",
        );
      } catch {}
    }
    const modelVision = (() => {
      const vision = detectVision(ctx);
      return vision === null ? null : settings.snap.enabled && vision;
    })();

    // Safety snapshot first — never blocks compaction.
    if (settings.snapshot.enabled) {
      try {
        const entries = ctx.sessionManager.getEntries() ?? [];
        writeSnapEntries(process.cwd(), entries, ev.reason ?? "unknown", Date.now(), settings.snapshot);
      } catch (error) {
        console.error("[ultracompress] snapshot failed:", error);
      }
    }

    const stdin = buildCompactStdin(ev, {
      policy: args.policy ?? settings.policy,
      keepUserTurns: args.keep ?? settings.keepUserTurns,
      smartKeepTail: settings.smartKeepTail,
      modelVision,
      snapMinChars: settings.snap.minChars,
    });

    // Automatic compaction caps both sides of the summary channel; an explicit
    // /ultracompress run is the operator asking for the full brief.
    if (!isExplicitUltraCompress && stdin.previousSummary) {
      stdin.previousSummary = capSummary(stdin.previousSummary, settings.summaryMaxBytes);
    }
    const rcArgs = ["compact", "--policy", stdin.policy, "--vision", stdin.vision as string];
    const res = await runUltraCompress<UltraCompressCompactResult>(ultracompressBin, rcArgs,
      {
        ...stdin,
        imageTokensPerFrame: settings.snap.imageTokensPerFrame,
        // The core fits the budget by dropping the oldest transcript lines;
        // the cap below is only the safety net for an older binary.
        ...(isExplicitUltraCompress ? {} : { summaryMaxBytes: settings.summaryMaxBytes }),
      }, 30_000);
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
    if (!isExplicitUltraCompress) mapped.summary = capSummary(mapped.summary, settings.summaryMaxBytes);

    try {
      ctx?.ui?.notify?.(formatStatsLine(rc), "info");
    } catch {}
    dbg({ compact: { reason: ev.reason, stats: rc.stats, summaryPreview: rc.summary.slice(0, 400) } });
    return { compaction: mapped };
  });

  // ── Commands ───────────────────────────────────────────────────────────
  pi.registerCommand("ultracompress", {
    description: "Compact now with UltraCompress (keep:N policy:auto|vcc|snap)",
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
        `ultracompress ${packageVersion()}`,
        `UltraCompress binary: ${ultracompressBin}${binAvailable ? "" : " (not found)"}`,
        `policy: ${settings.policy} · override: ${settings.overrideDefaultCompaction} · smart-keep: ${settings.smartKeepTail}`,
        `snap: ${settings.snap.enabled ? "on" : "off"} (placement: ${settings.snap.placement})`,
        `summary cap: ${settings.summaryMaxBytes} bytes · snapshots kept: ${settings.snapshot.maxCount} / ${settings.snapshot.maxAgeDays}d`,
        `transforms cached: ${transformCache.size}`,
        `snapshots: ${countSnaps(process.cwd())} in .steak-pi/snaps`,
      ];
      await ctx.ui.notify(lines.join("\n"), "info");
    },
  });

  pi.registerCommand("ultracompress-update", {
    description: "Install the UltraCompress CLI update, or: check | skip | auto on|off|status",
    handler: async (args, ctx) => {
      const argv = updateArgs(args ?? "");
      if (!argv) {
        await ctx.ui.notify("usage: /ultracompress-update [check | skip | auto on|off|status]", "warning");
        return;
      }
      const result = await runCli(ultracompressBin, argv, 300_000);
      const text = (result.stdout || result.stderr).trim() || `ultracompress update exited ${result.code}`;
      await ctx.ui.notify(text, result.code === 0 || result.code === 10 ? "info" : "warning");
      if (result.code === 0 && (argv[1] === "--yes" || argv[1] === "--skip")) ctx.ui.setStatus(UPDATE_STATUS_KEY, undefined);
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

}
