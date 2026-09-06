import "./ultracompress-settings-mock.ts";
import { describe, expect, it, vi } from "vitest";
import { recallArgs, parseRecallCommand, recallText } from "../src/recall.ts";
import { applyTransforms, collectCandidates, ucReplacement, type UcOp } from "../src/transforms.ts";
import ultraCompressExtension from "../index.ts";
import { runUltraCompress } from "../src/bridge.ts";
vi.mock("../src/bridge.ts", () => ({ runUltraCompress: vi.fn() }));
const session = { getSessionFile: () => "/tmp/current.jsonl", getLeafId: () => "current-tip" };

describe("session-local recall contract", () => {
  it("pins default recall to the actual current session and tip", () => {
    expect(recallArgs({ query: "alpha" }, session)).toEqual(["recall", "--session", "/tmp/current.jsonl", "--query", "alpha", "--scope", "lineage", "--leaf", "current-tip"]);
    expect(recallArgs({ query: "alpha" }, { ...session, getLeafId: () => null }).slice(-2)).toEqual(["--leaf", ""]);
  });
  it("never carries the current tip into an explicit other session or all branches", () => {
    const args = recallArgs({ query: "alpha", sessionFile: "/tmp/other session.jsonl" }, session);
    expect(args).toContain("/tmp/other session.jsonl"); expect(args).not.toContain("--leaf");
    const all = recallArgs({ query: "alpha", scope: "all" }, session);
    expect(all).toContain("/tmp/current.jsonl"); expect(all).not.toContain("--leaf");
  });
  it("propagates narrow filters and byte budgets", () => {
    const args = recallArgs({ query: "alpha", role: "toolResult", toolName: "read", afterEntry: "a", beforeEntry: "b", page: 2, perPage: 3, snippetBytes: 256, maxOutputBytes: 2048, regex: true }, session);
    for (const flag of ["--role", "--tool-name", "--after-entry", "--before-entry", "--page", "--per-page", "--snippet-bytes", "--max-output-bytes", "--regex"]) expect(args).toContain(flag);
  });
  it("rejects invalid parameters without silently widening", () => {
    for (const params of [{ scope: "global" }, { scope: null }, { sessionFile: null }, { constructor: true }, { perPage: 0 }, { page: 1.5 }, { maxOutputBytes: Infinity }, { role: "system" }, { toolName: "" }, { sessionFile: "" }, { other: true }, { regex: "true" }]) {
      expect(() => recallArgs({ query: "alpha", ...params }, session)).toThrow();
    }
    expect(() => recallArgs({ query: " " }, session)).toThrow();
    expect(() => recallArgs({ query: "alpha" }, { ...session, getSessionFile: () => undefined })).toThrow();
  });
  it("supports command JSON paths with spaces and legacy explicit scope", () => {
    expect(parseRecallCommand('alpha scope:all role:user perPage:2')).toEqual({ query: "alpha", scope: "all", role: "user", perPage: 2 });
    expect(parseRecallCommand('/alpha|beta/')).toEqual({ query: "alpha|beta", regex: true });
    expect(parseRecallCommand('{"query":"alpha","sessionFile":"/tmp/other session.jsonl"}').sessionFile).toBe("/tmp/other session.jsonl");
    expect(() => parseRecallCommand("alpha scope:all scope:lineage")).toThrow();
    for (const json of ['{"query":"alpha","scope":null}', '{"query":"alpha","sessionFile":null}', '{"query":"alpha","constructor":true}']) {
      expect(() => recallArgs(parseRecallCommand(json), session)).toThrow();
    }
  });
  it("enforces actual UTF-8 result bytes rather than character count", () => {
    expect(recallText({ x: "é" })).toBe('{"x":"é"}');
    expect(() => recallText({ x: "é".repeat(600) }, 1024)).toThrow();
  });
  it("registered command and tool share exact current-tip propagation", async () => {
    const tools = new Map<string, any>(); const commands = new Map<string, any>();
    ultraCompressExtension({ on() {}, registerTool(t: any) { tools.set(t.name, t); }, registerCommand(n: string, c: any) { commands.set(n, c); } } as never);
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: true, data: { session_id: "current", scope: "lineage", hits: [] } });
    const ctx = { sessionManager: session, ui: { notify: vi.fn() } };
    await tools.get("ultracompress_recall").execute("t", { query: "alpha" }, undefined, undefined, ctx);
    expect(vi.mocked(runUltraCompress).mock.lastCall?.[1]).toEqual(recallArgs({ query: "alpha" }, session));
    await commands.get("ultracompress-recall").handler("alpha role:user", ctx);
    expect(vi.mocked(runUltraCompress).mock.lastCall?.[1]).toEqual(recallArgs({ query: "alpha", role: "user" }, session));
  });
});

describe("fresh read break-even policy", () => {
  it("avoids archive+immediate-retrieval overhead even on a cached transform", () => {
    const text = "explicitly requested text ".repeat(200);
    const op: UcOp = { op: "uc", message_index: 0, block_index: 0, packet: "packet", stub: "stub", reference: "uc:" + "a".repeat(64), tokens_before: 1500, tokens_after: 100 };
    const cache = new Map([["k", { op, blocks: ucReplacement(op) }]]);
    const messages = [{ role: "assistant", content: [] }, { role: "toolResult", toolName: "read", content: [{ type: "text", text }] }];
    expect(collectCandidates(messages, 1200, () => "k")).toEqual([]);
    const result = applyTransforms(structuredClone(messages), cache, () => "k", "nextUser");
    expect(result.ucApplied).toBe(0); expect(result.messages[1].content).toEqual([{ type: "text", text }]);
    const older = [...messages, { role: "assistant", content: [] }];
    expect(collectCandidates(older, 1200, () => "k")).toHaveLength(1);
    expect(applyTransforms(structuredClone(older), cache, () => "k", "nextUser").ucApplied).toBe(1);
    // Immediate retrieval necessarily includes the full original PLUS marker
    // and another request. Passing through avoids this overhead exactly.
    expect(Buffer.byteLength(text + ucReplacement(op)[0].text)).toBeGreaterThan(Buffer.byteLength(text));
  });
});
