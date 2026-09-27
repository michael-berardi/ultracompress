import "./ultracompress-settings-mock.ts";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ultraCompressExtension from "../index.ts";
import { DEFAULT_SETTINGS, loadSettings, type UltraCompressSettings } from "../src/settings.ts";
import { runUltraCompress } from "../src/bridge.ts";
import * as transforms from "../src/transforms.ts";
import type { AgentLikeMessage, SnapOp } from "../src/transforms.ts";

vi.mock("../src/bridge.ts", () => ({ runUltraCompress: vi.fn() }));
vi.mock("../src/transforms.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/transforms.ts")>();
  return { ...actual, cacheKey: vi.fn(actual.cacheKey) };
});

const a = "A".repeat(7000);
const b = "B".repeat(7000);
const image = { type: "image", data: "original-base64", mimeType: "image/png" };
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
  { role: "assistant", content: [text("consumed")] },
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
    context: (input: AgentLikeMessage[], fresh = false) => handlers.get("context")![1]({
      messages: fresh || input.some((m) => m.role === "assistant") ? input :
        [...input, { role: "assistant", content: [text("consumed")] }],
    }, { model }),
    start: () => handlers.get("session_start")![0]({}, {}),
    compact: () => handlers.get("session_before_compact")![0]({}, {}),
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(runUltraCompress).mockReset();
});

describe("request-local snap transform keys", () => {
  it.each(["nextUser", "inline"] as const)("hashes unique text once and caches %s frames", async (placement) => {
    const settings = structuredClone(DEFAULT_SETTINGS);
    settings.snap.placement = placement;
    const hooks = register(settings);
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: { ops: [{ ...snap, message_index: 0 }], no_gain: [{ message_index: 1, block_index: 0 }] } });
    const original = messages();
    const result = await hooks.context(structuredClone(original));
    expect(result.messages[1].content).not.toEqual(original[1].content);
    expect(vi.mocked(transforms.cacheKey).mock.calls.map((call) => call[1])).toEqual([a, b]);
    expect(JSON.stringify(result.messages)).toContain("archive/frame-1");
    expect(result.messages[3].content).toBe(a);
    vi.mocked(transforms.cacheKey).mockClear();
    expect(await hooks.context(structuredClone(original))).toEqual(result);
    expect(transforms.cacheKey).toHaveBeenCalledTimes(2);
    expect(runUltraCompress).toHaveBeenCalledTimes(1);
  });

  it("leaves context unchanged on bridge failure and retries", async () => {
    const hooks = register(structuredClone(DEFAULT_SETTINGS));
    const original: AgentLikeMessage[] = [{ role: "toolResult", content: [text(a)] }];
    vi.mocked(runUltraCompress).mockResolvedValueOnce({ ok: false, error: "unavailable" });
    expect(await hooks.context(structuredClone(original))).toBeUndefined();
    vi.mocked(runUltraCompress).mockResolvedValueOnce({ ok: true, data: { ops: [{ ...snap, message_index: 0 }] } });
    expect((await hooks.context(structuredClone(original))).messages[0].content).not.toEqual(original[0].content);
    expect(runUltraCompress).toHaveBeenCalledTimes(2);
  });
});
