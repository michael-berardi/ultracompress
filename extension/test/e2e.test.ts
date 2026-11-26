import { describe, expect, it, vi } from "vitest";
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as url from "node:url";

/**
 * End-to-end: real UltraCompress binary against a realistic fixture session.
 * Skipped when the binary hasn't been built (cargo build --release).
 */

const here = path.dirname(url.fileURLToPath(import.meta.url));
const ULTRACOMPRESS = process.env.ULTRACOMPRESS_BIN ?? path.join(here, "..", "..", "target", "release", "ultracompress");
const hasBinary = fs.existsSync(ULTRACOMPRESS);

// The adapter must be driven with the real binary and inline placement; the
// factory stays lazy so the resolved path is read after this module loads.
vi.mock("../src/settings.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/settings.ts")>();
  return {
    ...actual,
    loadSettings: () => ({
      ...structuredClone(actual.DEFAULT_SETTINGS),
      policy: "auto" as const,
      snapshot: { ...structuredClone(actual.DEFAULT_SETTINGS.snapshot), enabled: false },
      snap: { ...structuredClone(actual.DEFAULT_SETTINGS.snap), placement: "inline" as const, minChars: 600 },
    }),
    resolveUltraCompressBin: () => ULTRACOMPRESS,
    isUltraCompressBinAvailable: (bin: string) => fs.existsSync(bin),
  };
});

const FIXTURE = [
  { type: "session", id: "fx", cwd: "/tmp/proj" },
  { type: "message", id: "e1", parentId: null, message: { role: "user", content: [{ type: "text", text: "Fix the build. Always run tests after fixes." }] } },
  { type: "message", id: "e2", parentId: "e1", message: { role: "assistant", content: [{ type: "toolCall", id: "c1", name: "bash", arguments: { command: "cargo build 2>&1" } }] } },
  {
    type: "message", id: "e3", parentId: "e2",
    message: {
      role: "toolResult", toolCallId: "c1", toolName: "bash",
      content: [{ type: "text", text: Array.from({ length: 300 }, (_, i) => `warning: unused variable row_${i} in src/lib.rs`).join("\n") }],
    },
  },
  { type: "message", id: "e4", parentId: "e3", message: { role: "assistant", content: [{ type: "text", text: "Found unused variables; fixing." }] } },
  { type: "message", id: "e5", parentId: "e4", message: { role: "user", content: [{ type: "text", text: "ok next run the tests" }] } },
  { type: "message", id: "e6", parentId: "e5", message: { role: "assistant", content: [{ type: "text", text: "Running tests now." }] } },
];

function runUltraCompress(args: string[], stdin?: unknown): any {
  return JSON.parse(
    execFileSync(ULTRACOMPRESS, args, { input: stdin ? JSON.stringify(stdin) : undefined, encoding: "utf8" }),
  );
}

describe.skipIf(!hasBinary)("UltraCompress binary e2e", () => {
  it("compacts a fixture session with a safe cut", () => {
    const r = runUltraCompress(["compact", "--policy", "auto", "--vision", "on", "--keep-default"], {
      entries: FIXTURE.filter((e) => e.type === "message" || e.type === "compaction"),
      tokensBefore: 12_000,
    });
    expect(r.summary).toContain("[Session Goal]");
    expect(r.summary).toContain("Fix the build");
    expect(r.summary).toContain("Always run tests");
    expect(r.first_kept_entry_id).toBe("e5");
    expect(r.stats.savings_pct).toBeGreaterThan(0);
  });

  it("transforms oversized tool results for the live path", () => {
    const r = runUltraCompress(["transform", "--policy", "auto", "--vision", "on"], {
      messages: [FIXTURE[3]],
      modelVision: true,
    });
    expect(r.stats.blocks_scanned).toBe(1);
    expect(r.stats.snap_ops).toBeGreaterThanOrEqual(0);
    // Either framed (vision) or explicitly kept as text; never both engines.
    expect(r.ops.length).toBeLessThanOrEqual(1);
  });

  it("honors adapter settings and previous summary across the real CLI boundary", () => {
    const payload = {
      entries: FIXTURE.filter((e) => e.type === "message"),
      previousSummary: "[Session Goal]\n- Preserve bridge sentinel goal\n",
      keepUserTurns: 0, smartKeepTail: false, policy: "vcc", vision: "off", modelVision: false,
    };
    const r = runUltraCompress(["compact"], payload);
    expect(r.summary).toContain("Preserve bridge sentinel goal");
    expect(r.details.previousSummaryUsed).toBe(true);
    expect(r.stats.keep_user_turns_resolved).toBe(0);
    const overridden = runUltraCompress(["compact", "--keep", "1"], payload);
    expect(overridden.stats.keep_user_turns_resolved).toBe(1);
  });

  it("honors snap threshold and CLI precedence", () => {
    const payload = { messages: [FIXTURE[3]], snapMinChars: 100000, modelVision: true, vision: "on" };
    expect(runUltraCompress(["transform"], payload).stats.blocks_scanned).toBe(0);
    expect(runUltraCompress(["transform", "--snap-min-chars", "1"], payload).stats.blocks_scanned).toBe(1);
  });

  it("recalls from a raw session file losslessly", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ultracompress-e2e-"));
    const file = path.join(dir, "session.jsonl");
    fs.writeFileSync(file, FIXTURE.map((e) => JSON.stringify(e)).join("\n") + "\n");
    const r = runUltraCompress(["recall", "--session", file, "--query", "unused variables"]);
    expect(r.total).toBeGreaterThanOrEqual(1);
    expect(r.hits[0].snippet).toContain("unused");
    fs.rmSync(dir, { recursive: true, force: true });
  });

});

describe.skipIf(!hasBinary)("adapter e2e with the real binary", () => {
  it("replaces a long tool result with decodable PNG frames", async () => {
    const { default: ultraCompressExtension } = await import("../index.ts");
    const handlers = new Map<string, (event: any, ctx: any) => any>();
    ultraCompressExtension({
      on(name: string, handler: any) { handlers.set(name, handler); },
      registerCommand() {}, registerTool() {},
    } as never);
    const long = Array.from({ length: 200 }, (_, i) => `warning: unused variable row_${i} in src/lib.rs`).join("\n");
    const messages = [
      { role: "user", content: "run the build" },
      { role: "toolResult", content: [{ type: "text", text: long }] },
      { role: "assistant", content: "done" },
    ];
    const result = await handlers.get("context")!({ messages }, { model: { provider: "anthropic", input: ["text", "image"] } });
    expect(result).toBeDefined();
    const content = result.messages[1].content as Array<Record<string, unknown>>;
    const images = content.filter((block) => block.type === "image");
    expect(images.length).toBeGreaterThan(0);
    const data = String(images[0].data ?? "");
    expect(data.length).toBeGreaterThan(0);
    expect(data).not.toBe("undefined");
    // Base64 of a real PNG must decode to the 8-byte PNG signature.
    expect([...Buffer.from(data, "base64").subarray(0, 8)]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  });
});
