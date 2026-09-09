import { describe, expect, it } from "vitest";
import {
  applyTransforms,
  cacheKey,
  collectCandidates,
  nextUserIndex,
  snapFrameBlocks,
  snapTextReplacement,
  ucReplacement,
  capCache,
  type AgentLikeMessage,
  type SnapOp,
  type UcOp,
} from "../src/transforms.ts";

const bigText = (n: number) =>
  Array.from({ length: n }, (_, i) => `line ${i}: some build output that keeps going`).join("\n");

function sessionWithToolResult(text: string): AgentLikeMessage[] {
  return [
    { role: "user", content: "run the build" },
    {
      role: "toolResult",
      content: [{ type: "text", text }],
    },
    { role: "assistant", content: "consumed" },
  ];
}

const ucOp: UcOp = {
  op: "uc",
  message_index: 0,
  block_index: 0,
  stub: "[UC packet: JSON payload, 2000 → 900 tokens, -55%]",
  packet: "@UC1 c=j\n…",
  tokens_before: 526,
  tokens_after: 237,
};

const snapOp: SnapOp = {
  op: "snap",
  message_index: 0,
  block_index: 0,
  head: "first lines",
  tail: "last lines",
  frames: [{ id: "f1", width: 648, height: 968, pngBase64: "aGVsbG8=" }],
  tokens_before: 2631,
  tokens_after: 837,
};

describe("collectCandidates", () => {
  it("finds toolResult text blocks above the floor", () => {
    const msgs = [
      ...sessionWithToolResult("x".repeat(7000)),
      { role: "toolResult", content: [{ type: "text", text: "tiny" }] },
    ];
    const c = collectCandidates(msgs, 6000, (t) => t.slice(0, 8));
    expect(c).toHaveLength(1);
    expect(c[0].blockIndex).toBe(0);
  });

  it("ignores non-toolResult roles", () => {
    const msgs = [{ role: "user", content: "x".repeat(9000) }];
    expect(collectCandidates(msgs, 6000, (t) => t.slice(0, 8))).toHaveLength(0);
  });

  it("handles string content", () => {
    const msgs = [{ role: "toolResult", content: "x".repeat(7000) }, { role: "assistant", content: "consumed" }];
    const c = collectCandidates(msgs, 6000, (t) => t.slice(0, 8));
    expect(c).toHaveLength(1);
  });
});

describe("applyTransforms", () => {
  it("replaces UC blocks inline", () => {
    const msgs = sessionWithToolResult("x".repeat(7000));
    const key = "k1";
    const result = applyTransforms(
      msgs,
      new Map([[key, { op: ucOp, blocks: ucReplacement(ucOp) }]]),
      (_m, _bi) => key,
      "nextUser",
    );
    expect(result.ucApplied).toBe(1);
    const content = msgs[1].content as Array<Record<string, unknown>>;
    expect(String(content[0].text)).toContain("@UC1");
  });

  it("routes snap frames to the next user message", () => {
    const msgs = [
      { role: "user", content: "run it" },
      { role: "toolResult", content: [{ type: "text", text: "x".repeat(7000) }] },
      { role: "assistant", content: "done" },
      { role: "user", content: "what happened?" },
    ];
    const key = "k2";
    const result = applyTransforms(
      msgs,
      new Map([[key, { op: snapOp, blocks: snapTextReplacement(snapOp) }]]),
      (_m, _bi) => key,
      "nextUser",
    );
    expect(result.snapApplied).toBe(1);
    const toolContent = msgs[1].content as Array<Record<string, unknown>>;
    expect(String(toolContent[0].text)).toContain("1 image frame(s)");
    // Frames land on the following user message.
    const userContent = msgs[3].content as Array<Record<string, unknown>>;
    const images = userContent.filter((b) => b.type === "image");
    expect(images).toHaveLength(1);
    expect(images[0].mimeType).toBe("image/png");
  });

  it("falls back to the previous user message when none follows", () => {
    const msgs = [
      { role: "user", content: "run it" },
      { role: "toolResult", content: [{ type: "text", text: "x".repeat(7000) }] },
      { role: "assistant", content: "consumed" },
    ];
    const key = "k3";
    applyTransforms(
      msgs,
      new Map([[key, { op: snapOp, blocks: snapTextReplacement(snapOp) }]]),
      () => key,
      "nextUser",
    );
    const userContent = msgs[0].content as Array<Record<string, unknown>>;
    expect(userContent.filter((b) => b.type === "image")).toHaveLength(1);
  });

  it("inline placement keeps frames in the tool result", () => {
    const msgs = sessionWithToolResult("x".repeat(7000));
    const key = "k4";
    applyTransforms(msgs, new Map([[key, { op: snapOp, blocks: snapTextReplacement(snapOp) }]]), () => key, "inline");
    const content = msgs[1].content as Array<Record<string, unknown>>;
    expect(content.some((b) => b.type === "image")).toBe(true);
  });
});

describe("misc", () => {
  it("nextUserIndex finds following then preceding user", () => {
    const msgs: AgentLikeMessage[] = [
      { role: "user", content: "a" },
      { role: "toolResult", content: [{ type: "text", text: "t" }] },
      { role: "assistant", content: "b" },
    ];
    expect(nextUserIndex(msgs, 1)).toBe(0); // no following user → previous
    msgs.push({ role: "user", content: "c" });
    expect(nextUserIndex(msgs, 1)).toBe(3);
  });

  it("cacheKey varies with context and content", () => {
    expect(cacheKey({ a: 1 }, "x")).not.toBe(cacheKey({ a: 2 }, "x"));
    expect(cacheKey({ a: 1 }, "x")).not.toBe(cacheKey({ a: 1 }, "y"));
    expect(cacheKey({ a: 1 }, "x")).toBe(cacheKey({ a: 1 }, "x"));
  });

  it("capCache drops oldest entries", () => {
    const cache = new Map<string, unknown>();
    for (let i = 0; i < 10; i++) cache.set(`k${i}`, i);
    capCache(cache, 5);
    expect(cache.size).toBe(5);
    expect(cache.has("k9")).toBe(true);
    expect(cache.has("k0")).toBe(false);
  });

  it("snapFrameBlocks prefix a caption", () => {
    const blocks = snapFrameBlocks(snapOp);
    expect(blocks[0].type).toBe("text");
    expect(blocks[1].type).toBe("image");
  });
});
