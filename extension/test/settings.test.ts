import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { pathToFileURL } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import {
  DEFAULT_SETTINGS,
  isUltraCompressBinAvailable,
  mergeSettings,
  moduleDir,
  packageVersion,
} from "../src/settings.ts";

const scratch: string[] = [];
afterEach(() => {
  for (const dir of scratch.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

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

  it("caps the automatic summary budget at a bounded default", () => {
    expect(mergeSettings({}).summaryMaxBytes).toBe(16384);
    expect(mergeSettings({ summaryMaxBytes: 100000 }).summaryMaxBytes).toBe(65536);
    expect(mergeSettings({ summaryMaxBytes: 1 }).summaryMaxBytes).toBe(1024);
  });

  it("decodes percent-encoded module paths", () => {
    const dir = path.join(os.tmpdir(), "uc dir with spaces");
    expect(moduleDir(pathToFileURL(path.join(dir, "settings.ts")).href)).toBe(dir);
  });

  it("reports the package version from package.json", () => {
    const pkg = JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { version: string };
    expect(packageVersion()).toBe(pkg.version);
    expect(packageVersion(pathToFileURL(path.join(os.tmpdir(), "absent", "x.ts")).href)).toBe("unknown");
  });
});

describe("binary availability", () => {
  it("accepts executables, resolves PATH entries and rejects missing binaries", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "uc-bin-"));
    scratch.push(dir);
    const bin = path.join(dir, "ultracompress-test-bin");
    fs.writeFileSync(bin, "#!/bin/sh\n");
    fs.chmodSync(bin, 0o755);
    expect(isUltraCompressBinAvailable(bin)).toBe(true);
    expect(isUltraCompressBinAvailable(path.join(dir, "missing"))).toBe(false);
    expect(isUltraCompressBinAvailable("ultracompress-test-bin", { PATH: dir })).toBe(true);
    expect(isUltraCompressBinAvailable("ultracompress-test-bin", { PATH: path.join(dir, "nowhere") })).toBe(false);
    expect(isUltraCompressBinAvailable("ultracompress-test-bin", {})).toBe(false);
    expect(isUltraCompressBinAvailable(dir)).toBe(false); // a directory is not a binary
  });
});
