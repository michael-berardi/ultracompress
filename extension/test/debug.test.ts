import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { debugPath, writeDebug } from "../src/debug.ts";

describe("optional debug dump", () => {
  it("is a single owner-only file in the private state directory", () => {
    const previous = process.env.XDG_STATE_HOME;
    const dir = mkdtempSync(join(tmpdir(), "ultracompress-debug-"));
    try {
      process.env.XDG_STATE_HOME = dir;
      const file = debugPath();
      expect(file.startsWith(dir)).toBe(true);
      writeDebug({ summaryPreview: "local preview" });
      expect(statSync(join(dir, "ultracompress")).mode & 0o777).toBe(0o700);
      expect(statSync(file).mode & 0o777).toBe(0o600);
      writeDebug({ ok: true });
      expect(JSON.parse(readFileSync(file, "utf8"))).toEqual({ ok: true });
    } finally {
      if (previous === undefined) delete process.env.XDG_STATE_HOME;
      else process.env.XDG_STATE_HOME = previous;
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
