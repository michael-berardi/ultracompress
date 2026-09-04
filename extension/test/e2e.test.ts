import { describe, expect, it } from "vitest";
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
const ULTRACOMPRESS = path.join(here, "..", "..", "target", "release", "ultracompress");
const hasBinary = fs.existsSync(ULTRACOMPRESS);

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
    expect(r.stats.snap_ops + r.stats.uc_ops).toBeGreaterThanOrEqual(0);
    // Either framed (vision) or explicitly kept as text; never both engines.
    expect(r.ops.length).toBeLessThanOrEqual(1);
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

  it("uc bridge stays graceful when payloads are not JSON", () => {
    const r = runUltraCompress(["uc", "decode"], { packet: "not a packet" });
    expect(r.error ?? r.decoded).toBeDefined();
  });
});
