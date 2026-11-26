import "./ultracompress-settings-mock.ts";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ultraCompressExtension from "../index.ts";
import { runUltraCompress } from "../src/bridge.ts";
import { capSummary } from "../src/compact-hook.ts";
import { DEFAULT_SETTINGS, loadSettings, mergeSettings } from "../src/settings.ts";

vi.mock("../src/bridge.ts", () => ({ runUltraCompress: vi.fn() }));
beforeEach(() => vi.clearAllMocks());

const summary = "日本語🙂".repeat(10000);
const marker = "\n[Summary capped; use ultracompress_recall for omitted history.]";

function expectCapped(value: string, budget: number) {
  expect(Buffer.byteLength(value)).toBeLessThanOrEqual(budget);
  expect(Buffer.byteLength(value)).toBeGreaterThan(budget - 4);
  expect(value.endsWith(marker)).toBe(true);
  expect(value).not.toContain("\ufffd");
  expect(summary.startsWith(value.slice(0, -marker.length))).toBe(true);
}

describe("automatic summary byte budget", () => {
  it("defaults to 16KiB and clamps configurable budgets", () => {
    expect(DEFAULT_SETTINGS.summaryMaxBytes).toBe(16384);
    for (const [requested, expected] of [[512, 1024], [2048, 2048], [100000, 65536], [NaN, 16384]]) {
      expect(mergeSettings({ summaryMaxBytes: requested }).summaryMaxBytes).toBe(expected);
    }
  });

  it.each([1024, 16384, 65536])("caps UTF-8 without splitting characters at %i bytes", (budget) => {
    expectCapped(capSummary(summary, budget), budget);
    const boundary = "é".repeat(budget / 2);
    expect(capSummary(boundary, budget)).toBe(boundary);
    expect(capSummary("short", budget)).toBe("short");
  });

  it.each([16384, 2048])("caps automatic input/output at %i bytes but preserves explicit compaction", async (budget) => {
    const settings = mergeSettings({ summaryMaxBytes: budget, snapshot: { enabled: false } });
    vi.mocked(loadSettings).mockReturnValue(settings);
    const handlers = new Map<string, any[]>();
    ultraCompressExtension({
      on(name: string, fn: any) { handlers.set(name, [...(handlers.get(name) ?? []), fn]); },
      registerTool() {}, registerCommand() {},
    } as never);
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: {
      summary, first_kept_entry_id: "kept", details: {},
      stats: { tokens_before_est: 10000, tokens_after_est: 1000, savings_pct: 90, summarized_messages: 4, kept_messages: 1 },
    } });
    const compact = handlers.get("session_before_compact")![0];
    const event = { preparation: { previousSummary: summary, tokensBefore: 10000 }, branchEntries: [] };
    const result = await compact(event, {});
    expectCapped(result.compaction.summary, budget);
    const payload = vi.mocked(runUltraCompress).mock.lastCall?.[2] as { previousSummary: string };
    expectCapped(payload.previousSummary, budget);
    expect(event.preparation.previousSummary).toBe(summary);
    const explicit = await compact({ ...event, customInstructions: "/ultracompress" }, {});
    expect(explicit.compaction.summary).toBe(summary);
    expect((vi.mocked(runUltraCompress).mock.lastCall?.[2] as { previousSummary: string }).previousSummary).toBe(summary);
  });
});
