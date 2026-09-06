import "./ultracompress-settings-mock.ts";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ultraCompressExtension from "../index.ts";
import { DEFAULT_SETTINGS, loadSettings, type UltraCompressSettings } from "../src/settings.ts";
import { runUltraCompress } from "../src/bridge.ts";
import * as transforms from "../src/transforms.ts";
import { UcReferences } from "../src/references.ts";
import type { AgentLikeMessage, SnapOp, UcOp } from "../src/transforms.ts";

vi.mock("../src/bridge.ts", () => ({ runUltraCompress: vi.fn() }));
vi.mock("../src/transforms.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/transforms.ts")>();
  return { ...actual, cacheKey: vi.fn(actual.cacheKey) };
});

const a = "A".repeat(7000);
const b = "B".repeat(7000);
const image = { type: "image", data: "original-base64", mimeType: "image/png" };
const uc: UcOp = {
  op: "uc", message_index: 0, block_index: 0,
  stub: "[UC packet: archive-handle]", packet: "@UC1\nexact-packet-bytes",
  tokens_before: 100, tokens_after: 10,
};
const snap: SnapOp = {
  op: "snap", message_index: 1, block_index: 0, head: "head\n", tail: "\ntail",
  frames: [
    { id: "archive/frame-1", width: 10, height: 20, pngBase64: "Zmlyc3Q=" },
    { id: "archive/frame-2", width: 20, height: 10, pngBase64: "c2Vjb25k" },
  ],
  tokens_before: 100, tokens_after: 10,
};
const text = (value: string) => ({ type: "text", text: value });
const messages = (): AgentLikeMessage[] => [
  { role: "user", content: a },
  { role: "toolResult", content: [image, text(a), text(b), text(a), text("tiny")] },
  { role: "toolResult", content: [text(a)] },
  { role: "toolResult", content: a }, // Existing string-content behavior stays unchanged.
  { role: "user", content: [text("next"), image] },
];

type Handler = (event: any, ctx: any) => any;
function register(settings: UltraCompressSettings, model = { provider: "anthropic", input: ["text", "image"] }) {
  vi.mocked(loadSettings).mockReturnValue(settings);
  const handlers = new Map<string, Handler[]>();
  ultraCompressExtension({
    on(name: string, handler: Handler) { handlers.set(name, [...(handlers.get(name) ?? []), handler]); },
    registerCommand() {}, registerTool() {},
  } as never);
  return {
    context: (input: AgentLikeMessage[]) => handlers.get("context")![1]({ messages: input }, { model }),
    compact: () => handlers.get("session_before_compact")![0]({}, {}),
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(runUltraCompress).mockReset();
});

describe("request-local live transform keys", () => {
  it.each(["nextUser", "inline"] as const)("hashes unique text once, preserving %s output and repeat occurrences", async (placement) => {
    const settings = structuredClone(DEFAULT_SETTINGS);
    settings.snap.placement = placement;
    const hooks = register(settings);
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: { ops: [uc, snap] } });
    const original = messages();
    const expected = structuredClone(original);
    // Unoptimized application oracle: original text lookup and unchanged transforms.
    transforms.applyTransforms(expected, new Map([
      [a, { op: { ...uc, reference: new UcReferences().put(a) }, blocks: [] }], [b, { op: snap, blocks: [] }],
    ]), (m, bi) => typeof m.content === "string" ? m.content : m.content[bi]?.text as string, placement);

    const result = await hooks.context(structuredClone(original));
    expect(result.messages).toEqual(expected);
    expect(JSON.stringify(result.messages)).toBe(JSON.stringify(expected));
    expect(vi.mocked(transforms.cacheKey).mock.calls.map((call) => call[1])).toEqual([a, b]);
    // Five candidate occurrences, four array blocks applied: baseline 9 hashes, now 2.
    const payload = vi.mocked(runUltraCompress).mock.calls[0][2] as any;
    expect(payload.messages.map((m: any) => m.message.content[0].text)).toEqual([a, b, a, a, a]);
    expect(payload.messages.map((m: any) => m.id)).toEqual(["rc1", "rc1", "rc1", "rc2", "rc3"]);
    expect(JSON.stringify(result.messages)).toContain("uc:");
    expect(JSON.stringify(result.messages)).not.toContain("@UC1\\nexact-packet-bytes");
    expect(JSON.stringify(result.messages)).toContain("archive/frame-1, archive/frame-2");
    expect(result.messages[3].content).toBe(a);

    vi.mocked(transforms.cacheKey).mockClear();
    expect(await hooks.context(structuredClone(original))).toEqual(result);
    expect(transforms.cacheKey).toHaveBeenCalledTimes(2); // New request, including cache hits.
    expect(runUltraCompress).toHaveBeenCalledTimes(1);
  });

  it("performs one rather than two hashes for a single eligible block", async () => {
    const hooks = register(structuredClone(DEFAULT_SETTINGS));
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: { ops: [uc] } });
    const result = await hooks.context([{ role: "toolResult", content: [text(a)] }]);
    expect(transforms.cacheKey).toHaveBeenCalledTimes(1);
    expect(result.messages[0].content).toEqual(transforms.ucReplacement({ ...uc, reference: new UcReferences().put(a) }));
  });

  it("preserves duplicate response precedence and fallback on bridge failure", async () => {
    const hooks = register(structuredClone(DEFAULT_SETTINGS));
    const input = () => [{ role: "toolResult", content: [text(a), text(a)] }];
    vi.mocked(runUltraCompress).mockResolvedValueOnce({ ok: false, error: "cancelled or unavailable" });
    const untouched = input();
    expect(await hooks.context(untouched)).toBeUndefined();
    expect(untouched).toEqual(input());
    expect(transforms.cacheKey).toHaveBeenCalledTimes(1);
    vi.mocked(runUltraCompress).mockResolvedValueOnce({ ok: true, data: {
      ops: [uc, { ...uc, message_index: 1, packet: "@UC1 last occurrence" }],
    } });
    const result = await hooks.context(input());
    expect(result.messages[0].content).toEqual([
      ...transforms.ucReplacement({ ...uc, reference: new UcReferences().put(a) }),
      ...transforms.ucReplacement({ ...uc, reference: new UcReferences().put(a) }),
    ]);
  });

  it("recomputes exact settings/calibration-sensitive keys on subsequent requests", async () => {
    const settings = structuredClone(DEFAULT_SETTINGS);
    settings.snapshot.enabled = false;
    const hooks = register(settings);
    const input = () => [{ role: "toolResult", content: [text(a)] }];
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: { ops: [uc] } });
    await hooks.context(input());
    expect(transforms.cacheKey).toHaveBeenLastCalledWith({ p: "auto", v: true, s: 6000, u: 1200, cpt: undefined }, a);
    const first = vi.mocked(transforms.cacheKey).mock.results[0].value;
    settings.policy = "uc";
    settings.uc.minChars = 1000;
    settings.snap.minChars = 5000;
    await hooks.context(input());
    expect(transforms.cacheKey).toHaveBeenLastCalledWith({ p: "uc", v: true, s: 5000, u: 1000, cpt: undefined }, a);
    expect(vi.mocked(transforms.cacheKey).mock.results[1].value).not.toBe(first);
    vi.mocked(runUltraCompress).mockResolvedValueOnce({ ok: true, data: { stats: { calibrated: true, chars_per_token: 3.5 } } });
    await hooks.compact();
    await hooks.context(input());
    expect(transforms.cacheKey).toHaveBeenLastCalledWith({ p: "uc", v: true, s: 5000, u: 1000, cpt: 3.5 }, a);
    expect(transforms.cacheKey).toHaveBeenCalledTimes(3);
    expect(runUltraCompress).toHaveBeenCalledTimes(4);
  });

  it("preserves existing model vision gating and latch across requests", async () => {
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: { ops: [uc] } });
    const model = { provider: "anthropic", input: ["text", "image"] };
    const settings = structuredClone(DEFAULT_SETTINGS);
    const hooks = register(settings, model);
    const input = () => [{ role: "toolResult", content: [text(a)] }];
    await hooks.context(input());
    const first = vi.mocked(transforms.cacheKey).mock.results[0].value;
    model.input = ["text"];
    await hooks.context(input());
    expect(vi.mocked(transforms.cacheKey).mock.results[1].value).toBe(first); // Existing visionKnown latch.
    const other = register(settings, model);
    await other.context(input());
    expect(transforms.cacheKey).toHaveBeenLastCalledWith({ p: "auto", v: false, s: 6000, u: 1200, cpt: undefined }, a);
    expect(vi.mocked(transforms.cacheKey).mock.results[2].value).not.toBe(first);
    expect(runUltraCompress).toHaveBeenCalledTimes(2);
  });
});
