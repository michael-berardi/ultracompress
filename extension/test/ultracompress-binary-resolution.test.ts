import { afterEach, expect, it, vi } from "vitest";
vi.mock("node:fs", async (importOriginal) => ({
  ...await importOriginal<typeof import("node:fs")>(),
  accessSync: vi.fn(),
}));
import os from "node:os";
import path from "node:path";
import { DEFAULT_SETTINGS, resolveUltraCompressBin } from "../src/settings";

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllEnvs(); });
it("prefers the versioned app-managed bridge over older user-local binaries", () => {
  vi.stubEnv("ULTRACOMPRESS_BIN", "");
  expect(resolveUltraCompressBin({ ...DEFAULT_SETTINGS, ultracompressBin: "" })).toBe(path.join(os.homedir(), ".ultraterm/bin/ultracompress"));
});
it("preserves an explicit bridge override", () => {
  vi.stubEnv("ULTRACOMPRESS_BIN", "/test/explicit-bridge");
  expect(resolveUltraCompressBin(DEFAULT_SETTINGS)).toBe("/test/explicit-bridge");
});
