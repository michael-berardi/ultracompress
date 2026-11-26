import { describe, expect, it } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { footerText, runCli, updateArgs } from "../src/update-notice.ts";
import { DEFAULT_SETTINGS, mergeSettings } from "../src/settings.ts";

describe("update footer notice", () => {
  it("shows one footer line only when the CLI reports a due notice", () => {
    expect(footerText('{"current":"0.8.0","latest":"0.9.0"}\n')).toBe(
      "ultracompress 0.9.0 available (you have 0.8.0) · run: /ultracompress-update",
    );
    expect(footerText("{}\n")).toBeUndefined();
    expect(footerText("")).toBeUndefined();
    expect(footerText("not json")).toBeUndefined();
    expect(footerText('{"latest":9}')).toBeUndefined();
  });

  it("maps the slash command to CLI arguments; no argument installs", () => {
    expect(updateArgs("")).toEqual(["update", "--yes"]);
    expect(updateArgs("install")).toEqual(["update", "--yes"]);
    expect(updateArgs(" check ")).toEqual(["update", "--check"]);
    expect(updateArgs("skip")).toEqual(["update", "--skip"]);
    expect(updateArgs("auto")).toEqual(["update", "--auto", "status"]);
    expect(updateArgs("auto on")).toEqual(["update", "--auto", "on"]);
    expect(updateArgs("auto maybe")).toBeUndefined();
    expect(updateArgs("upgrade")).toBeUndefined();
  });

  it("runs the CLI with piped stdio and keeps its exit code", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "uc-notice-test-"));
    const fake = path.join(dir, "ultracompress");
    fs.writeFileSync(
      fake,
      [
        "#!/bin/sh",
        // A terminal would let the CLI print its own notice; the footer path must never have one.
        'if [ -t 1 ] || [ -t 2 ]; then echo "terminal attached" >&2; exit 3; fi',
        'case "$*" in',
        '  "update --notice") echo \'{"current":"0.8.0","latest":"0.9.0"}\';;',
        '  "update --check") echo "ultracompress 0.9.0 available (you have 0.8.0)"; exit 10;;',
        '  *) echo "unexpected: $*" >&2; exit 2;;',
        "esac",
        "",
      ].join("\n"),
      { mode: 0o755 },
    );
    try {
      const notice = await runCli(fake, ["update", "--notice"]);
      expect(notice.code).toBe(0);
      expect(footerText(notice.stdout)).toContain("0.9.0 available");
      expect((await runCli(fake, ["update", "--check"])).code).toBe(10);
      const other = await runCli(fake, ["update", "--bogus"]);
      expect(other.code).toBe(2);
      expect(other.stderr).toContain("unexpected");
      const missing = await runCli(path.join(dir, "absent"), ["update", "--notice"]);
      expect(missing.code).toBeNull();
      expect(footerText(missing.stdout)).toBeUndefined();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it("keeps the footer switch in Pi settings, on by default", () => {
    expect(DEFAULT_SETTINGS.checkForUpdates).toBe(true);
    expect(mergeSettings({ checkForUpdates: false }).checkForUpdates).toBe(false);
    expect(mergeSettings({ checkForUpdates: "no" }).checkForUpdates).toBe(true);
  });
});
