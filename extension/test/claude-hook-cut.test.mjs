import { describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import * as url from "node:url";
import { handleCompact } from "../../claude-code/hooks/ultracompress.mjs";

/**
 * The Claude Code hook against the real binary: when the kept tail must move
 * forward to a clean user message, nothing between the binary's cut and that
 * message may disappear from both the summary and the kept tail.
 */

const here = path.dirname(url.fileURLToPath(import.meta.url));
const ULTRACOMPRESS = process.env.UC_TEST_BIN ?? path.join(here, "..", "..", "target", "release", "ultracompress");
const hasBinary = fs.existsSync(ULTRACOMPRESS);

function hookHost() {
  return {
    env: { get: async (name) => (name === "HOME" ? "/nonexistent-home" : undefined) },
    fs: { stat: async () => ({ kind: "file" }) },
    process: {
      run: async (argv, init) => {
        try {
          const stdout = execFileSync(ULTRACOMPRESS, argv.slice(1), {
            input: init.stdin,
            encoding: "utf8",
            env: { ...process.env, PATH: "" },
          });
          return { exitCode: 0, stdout, stderr: "" };
        } catch (error) {
          return { exitCode: error.status ?? 1, stdout: error.stdout ?? "", stderr: error.stderr ?? "" };
        }
      },
    },
    ui: { log: () => {}, toast: () => {} },
  };
}

describe.skipIf(!hasBinary)("Claude Code hook cut adjustment", () => {
  it("keeps a requirement sent with a tool result when the cut moves forward", async () => {
    const messages = [
      { role: "user", text: "Fix the flaky build in the payments service", toolUses: [] },
      { role: "assistant", text: "Looking.", toolUses: [{ tool_use_id: "t1", tool: "bash", input: { command: "make test" } }] },
      {
        role: "user",
        text: "Requirement: never touch the prod database",
        toolUses: [],
        toolResults: [{ tool_use_id: "t1", text: "build log line\n".repeat(400), isError: false }],
      },
      { role: "assistant", text: "Understood, avoiding prod.", toolUses: [] },
      { role: "user", text: "Now also update the changelog", toolUses: [] },
      { role: "assistant", text: "Done.", toolUses: [] },
    ];
    const out = await handleCompact(hookHost(), { trigger: "manual", instructions: "keep:2", messages }, async () => {
      throw new Error("stock compaction must not run");
    });
    const [summary, ...kept] = out.messages;
    expect(kept).toEqual(messages.slice(4));
    expect(summary.text).toContain("4 messages condensed");
    expect(summary.text).toContain("never touch the prod database");
  });
});
