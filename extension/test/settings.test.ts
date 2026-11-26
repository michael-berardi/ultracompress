import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { loadSettings, mergeSettings, DEFAULT_SETTINGS } from "../src/settings.ts";

describe("settings", () => {
  it("defaults survive a bad payload", () => {
    expect(mergeSettings(null)).toEqual(DEFAULT_SETTINGS);
    expect(mergeSettings("junk")).toEqual(DEFAULT_SETTINGS);
    expect(mergeSettings(42)).toEqual(DEFAULT_SETTINGS);
  });

  it("merges partial user config over defaults", () => {
    const s = mergeSettings({
      policy: "vcc",
      debug: true,
      snap: { placement: "inline", minChars: 10_000 },
    });
    expect(s.policy).toBe("vcc");
    expect(s.debug).toBe(true);
    expect(s.snap.placement).toBe("inline");
    expect(s.snap.minChars).toBe(10_000);
    expect(s.snapshot.enabled).toBe(true); // untouched
  });

  it("coerces keepUserTurns to a non-negative integer", () => {
    expect(mergeSettings({ keepUserTurns: 3.7 }).keepUserTurns).toBe(3);
    expect(mergeSettings({ keepUserTurns: -2 }).keepUserTurns).toBe(0);
    expect(mergeSettings({ keepUserTurns: null }).keepUserTurns).toBeNull();
  });

  it("rejects unknown policy values", () => {
    expect(mergeSettings({ policy: "turbo" }).policy).toBe("auto");
  });

  it("scaffolds settings privately", () => {
    const dir = mkdtempSync(join(tmpdir(), "ultracompress-settings-"));
    try {
      const file = join(dir, "agent", "ultracompress.json");
      loadSettings(file);
      expect(statSync(join(dir, "agent")).mode & 0o777).toBe(0o700);
      expect(statSync(file).mode & 0o777).toBe(0o600);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
