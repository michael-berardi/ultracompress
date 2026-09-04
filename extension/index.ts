import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { runUltraCompress } from "./src/bridge";
import { loadSettings, resolveUltraCompressBin, type UltraCompressSettings } from "./src/settings";
import { buildSnap, listSnaps, writeSnap } from "./src/snapshot";
import { adaptPayloadForZai, payloadHasDataUrlImages } from "./src/payload";
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

  // Snap-frame wire-format adapter: z.ai's coding endpoint expects image
  // payloads as `file` parts, not OpenAI `image_url` parts. Rewrite in place;
  // other providers are untouched. If frames ever get rejected despite this,
  // the agent_end handler below disables them for the rest of the session.
  pi.on("before_provider_request", (event, ctx) => {
    try {
      const model = ctx?.model as { provider?: string; baseUrl?: string } | undefined;
      const isZai = model?.provider === "zai" || String(model?.baseUrl ?? "").includes("z.ai");
      if (!isZai) return undefined;
      if (!payloadHasDataUrlImages(event.payload as { messages?: Array<{ content?: unknown }> })) return undefined;
      const n = adaptPayloadForZai(event.payload as { messages?: Array<{ content?: unknown }> });
      if (n > 0) dbg({ zaiFilePartsConverted: n });
    } catch {}
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
    const candidates = collectCandidates(messages, minChars, keyFor);
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

    const keyFn = (m: AgentLikeMessage, bi: number): string | undefined => {
      if (m.role !== "toolResult") return undefined;
      const content = m.content;
      if (typeof content === "string") return content.length >= minChars ? keyFor(content) : undefined;
      const block = content?.[bi] as { text?: unknown } | undefined;
      if (!block || typeof block.text !== "string") return undefined;
      return block.text.length >= minChars ? keyFor(block.text) : undefined;
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
        const now = Date.now();
        const { meta, payload } = buildSnap(entries, ev.reason ?? "unknown", now);
        writeSnap(process.cwd(), payload, meta.file);
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
      const query = args?.trim();
      if (!query) {
        await ctx.ui.notify("usage: /ultracompress-recall <keywords | /regex/> [scope:all]", "info");
        return;
      }
      const sessionFile = ctx.sessionManager.getSessionFile();
      if (!sessionFile) {
        await ctx.ui.notify("No session file to search.", "warning");
        return;
      }
      const scopeAll = /(?:^|\s)scope:all(?=\s|$)/.test(query);
      const q = query.replace(/(?:^|\s)scope:all(?=\s|$)/, "").trim();
      const res = await runUltraCompress(ultracompressBin, [
        "recall", "--session", sessionFile, "--query", q,
        ...(scopeAll ? ["--scope", "all"] : []), "--per-page", "8",
      ], null);
      if (!res.ok || !res.data) {
        await ctx.ui.notify(`UltraCompress recall failed: ${res.error}`, "error");
        return;
      }
      const r = res.data as { total: number; page: number; page_count: number; hits: Array<{ role: string; score: number; snippet: string; matched_terms: string[] }> };
      const header = `${r.total} hit(s) · page ${r.page}/${r.page_count}`;
      const body = r.hits
        .map((h, i) => `${i + 1}. [${h.role}] ${h.snippet}`)
        .join("\n\n");
      await ctx.ui.notify(`${header}\n\n${body || "No hits."}`, "info");
    },
  });

  pi.registerCommand("ultracompress-stats", {
    description: "UltraCompress status and settings",
    handler: async (_args, ctx) => {
      const lines = [
        `ultracompress ${"0.1.0"}`,
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
      "Recall earlier parts of this session — decisions, files, commands — including everything dropped by " +
      "compaction. The raw history stays on disk and searchable: reach for this BEFORE telling the user you " +
      "no longer have context. Plain keywords rank by relevance; a regex pattern also works. Results are paged.",
    promptSnippet:
      "ultracompress_recall: search this session's full raw history (compacted turns included) before saying context is lost. Plain keywords work best; scope:'all' widens the search.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "Keywords (e.g. 'redis cache decision') or a regex pattern." },
        scope: { type: "string", enum: ["lineage", "all"], description: "Default 'lineage' = active conversation path. 'all' includes other branches (edited/retried turns)." },
        page: { type: "number", description: "1-based page number. Default 1." },
      },
      required: ["query"],
    },
    async execute(_id, params, _signal, _onUpdate, ctx) {
      const sessionFile = ctx.sessionManager.getSessionFile();
      if (!sessionFile) {
        return { content: [{ type: "text", text: "No session file available to search." }], details: {} };
      }
      const query = String(params.query ?? "");
      const scopeAll = params.scope === "all";
      const page = typeof params.page === "number" && params.page >= 1 ? Math.floor(params.page) : 1;
      const res = await runUltraCompress(ultracompressBin, [
        "recall", "--session", sessionFile, "--query", query,
        ...(scopeAll ? ["--scope", "all"] : []), "--page", String(page),
      ], null);
      if (!res.ok || !res.data) {
        return { content: [{ type: "text", text: `UltraCompress recall failed: ${res.error}` }], details: {} };
      }
      const r = res.data as { total: number; page: number; page_count: number; hits: Array<{ entry_id: string; role: string; score: number; snippet: string; matched_terms: string[] }> };
      const lines = [
        `${r.total} hit(s) · page ${r.page}/${r.page_count} · scope ${scopeAll ? "all" : "lineage"}`,
        "",
        ...r.hits.map(
          (h, i) =>
            `${i + 1}. [${h.role}] ${h.snippet}\n   (entry ${h.entry_id}, matched: ${h.matched_terms.join(", ")})`,
        ),
        ...(r.hits.length === 0 ? ["No hits. Try fewer or different keywords."] : []),
      ];
      return { content: [{ type: "text", text: lines.join("\n") }], details: {} };
    },
  });

  pi.registerTool({
    name: "ultracompress_uc",
    label: "UC Decode",
    description:
      "Decode an UltraCompress UC packet back to exact JSON. UC packets appearing in context (marked " +
      "'[UC packet…decode via ultracompress_uc decode]') are lossless compressed JSON; pass the packet text to recover " +
      "the original payload verbatim.",
    promptSnippet: "ultracompress_uc: decode a UC packet (lossless compressed JSON) back to exact JSON when its detail is needed.",
    parameters: {
      type: "object",
      properties: {
        packet: { type: "string", description: "The full UC packet text, starting with @UC1." },
      },
      required: ["packet"],
    },
    async execute(_id, params) {
      const packet = String(params.packet ?? "");
      if (!packet) return { content: [{ type: "text", text: "No packet provided." }], details: {} };
      const res = await runUltraCompress<{ decoded?: string; error?: string }>(
        ultracompressBin, ["uc", "decode"], { packet },
      );
      if (!res.ok || !res.data) {
        return { content: [{ type: "text", text: `UltraCompress UC decode failed: ${res.error}` }], details: {} };
      }
      if (res.data.error) {
        return { content: [{ type: "text", text: `decode error: ${res.data.error}` }], details: {} };
      }
      return { content: [{ type: "text", text: res.data.decoded ?? "(empty)" }], details: {} };
    },
  });
}
