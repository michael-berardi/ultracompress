import { describe, expect, it, vi } from "vitest";
import ultraCompressExtension from "../index.ts";

// Optional real-binary test: no home settings, sessions, tmux, or provider calls.
vi.mock("../src/settings.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/settings.ts")>();
  return {
    ...actual,
    loadSettings: () => ({ ...structuredClone(actual.DEFAULT_SETTINGS), debug: false }),
    resolveUltraCompressBin: () => process.env.UC_TEST_BIN ?? "missing-test-binary",
  };
});

describe.skipIf(!process.env.UC_TEST_BIN)("real UC bridge retrieval", () => {
  it("transforms text through the real binary, retrieves it exactly, and leaves retrieval readable", async () => {
    const hooks = new Map<string, any[]>(); const tools = new Map<string, any>();
    ultraCompressExtension({
      on(name: string, handler: any) { hooks.set(name, [...(hooks.get(name) ?? []), handler]); },
      registerTool(tool: any) { tools.set(tool.name, tool); }, registerCommand() {},
    } as never);
    const original = ('build report: α β 日本語 café [invalid] "quoted"\n' + 'status: complete; file: src/component.ts\n').repeat(160);
    const ctx = { model: { provider: "zai", input: ["text"] } };
    const context = hooks.get("context")![1];
    const transformed = await context({ messages: [{ role: "toolResult", toolName: "read", content: [{ type: "text", text: original }] }] }, ctx);
    expect(transformed).toBeDefined();
    const marker = transformed.messages[0].content[0].text;
    expect(marker).not.toContain("@UC1");
    expect(marker.length).toBeLessThan(original.length);
    const ref = marker.match(/uc:[a-f0-9]{64}/)![0];
    const result = await tools.get("ultracompress_uc").execute("test", { packet: ref });
    expect(result.content[0].text).toBe(original);
    const messages = [{ role: "toolResult", toolName: "ultracompress_uc", content: result.content }];
    expect(await context({ messages }, ctx)).toBeUndefined();
    expect(messages[0].content[0].text).toBe(original);
  }, 15000);
});
