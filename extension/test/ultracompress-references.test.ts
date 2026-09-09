import "./ultracompress-settings-mock.ts";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ultraCompressExtension from "../index.ts";
import { runUltraCompress } from "../src/bridge.ts";
import { UcReferences } from "../src/references.ts";
import { applyTransforms, collectCandidates, ucReplacement, type UcOp } from "../src/transforms.ts";

vi.mock("../src/bridge.ts", () => ({ runUltraCompress: vi.fn() }));
const text = 'text with unicode: café 日本語, quotes " and newlines\n'.repeat(100);
const op: UcOp = { op: "uc", message_index: 0, block_index: 0, packet: "@UC1\nlegacy bytes", stub: "legacy stub", tokens_before: 1500, tokens_after: 100 };

beforeEach(() => vi.mocked(runUltraCompress).mockReset());

function register() {
  const handlers = new Map<string, Array<(event: any, ctx?: any) => any>>();
  const tools = new Map<string, any>();
  ultraCompressExtension({
    on(name: string, fn: any) { handlers.set(name, [...(handlers.get(name) ?? []), fn]); },
    registerTool(tool: any) { tools.set(tool.name, tool); }, registerCommand() {},
  } as never);
  return { handlers, tools };
}

describe("UC original-output references", () => {
  it("is exact, deterministic, bounded and rejects paths or foreign ids", () => {
    const refs = new UcReferences(100, 2);
    const a = refs.put("alpha")!;
    expect(a).toMatch(/^uc:[a-f0-9]{64}$/);
    expect(refs.put("alpha")).toBe(a);
    expect(refs.get(a)).toBe("alpha");
    refs.put("beta"); refs.put("gamma");
    expect(refs.get(a)).toBeUndefined();
    expect(refs.put("x".repeat(101))).toBeUndefined();
    expect(refs.get("../../secret")).toBeUndefined();
    refs.clear(); expect(refs.get(refs.put("new")!)).toBe("new");
  });

  it("does not recompress decoded or recalled output, even on cache hits", () => {
    for (const toolName of ["ultracompress_uc", "ultracompress_recall"]) {
      const messages = [{ role: "toolResult", toolName, content: [{ type: "text", text }] }];
      expect(collectCandidates(messages, 1200, () => "key")).toEqual([]);
      const cache = new Map([["key", { op, blocks: ucReplacement(op) }]]);
      const result = applyTransforms(messages, cache, () => "key", "nextUser");
      expect(result.ucApplied).toBe(0);
      expect(result.messages[0].content).toEqual([{ type: "text", text }]);
    }
  });

  it("retrieves by short reference without copying or decoding dense text; clears on session replacement", async () => {
    const { handlers, tools } = register();
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: { ops: [{ ...op }] } });
    const original = [
      { role: "toolResult", toolName: "bash", content: [{ type: "text", text }] },
      { role: "assistant", content: [{ type: "text", text: "consumed" }] },
    ];
    const context = handlers.get("context")![1];
    const transformed = await context({ messages: structuredClone(original) }, { model: { provider: "zai" } });
    const marker = transformed.messages[0].content[0].text;
    const ref = marker.match(/uc:[a-f0-9]{64}/)![0];
    expect(marker).not.toContain("@UC1");
    expect(marker).toContain("deferred retrieval");
    expect(original[0].content[0].text).toBe(text);
    const tool = tools.get("ultracompress_uc");
    const recovered = await tool.execute("call", { packet: ref });
    expect(recovered.content[0].text).toBe(text);
    expect(runUltraCompress).toHaveBeenCalledTimes(1);
    const decoded = [{ role: "toolResult", toolName: "ultracompress_uc", content: recovered.content }];
    expect(await context({ messages: decoded }, { model: { provider: "zai" } })).toBeUndefined();
    expect(decoded[0].content[0].text).toBe(text);
    for (const fn of handlers.get("session_start")!) fn({ reason: "new" });
    expect((await tool.execute("call", { packet: ref })).content[0].text).toContain("unavailable");
  });

  it("retains legacy complete-packet decode and gives actionable failure guidance", async () => {
    const { tools } = register(); const tool = tools.get("ultracompress_uc");
    vi.mocked(runUltraCompress).mockResolvedValueOnce({ ok: true, data: { decoded: '{"a":1}' } });
    expect((await tool.execute("c", { packet: "@UC1 complete" })).content[0].text).toBe('{"a":1}');
    vi.mocked(runUltraCompress).mockResolvedValueOnce({ ok: true, data: { error: "token id 58 not in codebook" } });
    const result = await tool.execute("c", { packet: "@UC1\n[invalid]" });
    expect(result.content[0].text).toContain("ultracompress_recall");
    expect(result.content[0].text).not.toContain("unrecoverable");
  });
});
