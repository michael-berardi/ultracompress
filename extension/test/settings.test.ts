import { describe, expect, it } from "vitest";
import { mergeSettings, DEFAULT_SETTINGS } from "../src/settings.ts";

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
      uc: { enabled: false, bin: "/opt/uc" },
      snap: { placement: "inline", minChars: 10_000 },
    });
    expect(s.policy).toBe("vcc");
    expect(s.debug).toBe(true);
    expect(s.uc.enabled).toBe(false);
    expect(s.uc.bin).toBe("/opt/uc");
    expect(s.uc.minChars).toBe(DEFAULT_SETTINGS.uc.minChars); // untouched
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
});
