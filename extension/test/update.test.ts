import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  CHECK_INTERVAL_MS, DEFAULT_UPDATE_STATE, UpdateState, USER_AGENT,
  checkDue, checkReport, fetchRelease, hostAllowed, isNewer, latestReleaseUrl,
  loadUpdateState, markShown, notice, parseRelease, runCheck, saveUpdateState, showable,
} from "../src/update.ts";
import { PACKAGE_VERSION, REPOSITORY } from "../src/metadata.ts";
import { DEFAULT_SETTINGS, mergeSettings } from "../src/settings.ts";

const releaseBody = JSON.stringify({
  tag_name: "v9.9.9", draft: false, prerelease: false, body: "First line\n\nSecond line",
  html_url: "https://example.invalid/release",
});

function fakeFetch(body: string, status = 200) {
  const calls: Array<{ url: string; init: any }> = [];
  const impl = async (url: string, init: any) => {
    calls.push({ url, init });
    return { ok: status >= 200 && status < 300, status, text: async () => body };
  };
  return { impl, calls };
}

describe("update contract (Pi adapter)", () => {
  it("compares only stable x.y.z versions", () => {
    expect(isNewer("0.6.8", PACKAGE_VERSION)).toBe(true);
    expect(isNewer("0.6.7", PACKAGE_VERSION)).toBe(false);
    expect(isNewer("garbage", PACKAGE_VERSION)).toBe(false);
    expect(isNewer("0.6.8-rc.1", PACKAGE_VERSION)).toBe(false);
  });

  it("parses only stable releases and derives the source from metadata", () => {
    expect(parseRelease(releaseBody)).toEqual({ version: "9.9.9", notes: "First line\nSecond line" });
    expect(() => parseRelease(JSON.stringify({ tag_name: "v9.9.9", prerelease: true }))).toThrow("prerelease");
    expect(() => parseRelease(JSON.stringify({ tag_name: "v9.9.9", draft: true }))).toThrow("draft");
    expect(() => parseRelease(JSON.stringify({ tag_name: "nightly" }))).toThrow("stable");
    expect(latestReleaseUrl()).toBe(`https://api.github.com/repos/${REPOSITORY.split("/")[3]}/${REPOSITORY.split("/")[4]}/releases/latest`);
  });

  it("makes one anonymous GET with a fixed user agent and a four-second cap", async () => {
    const { impl, calls } = fakeFetch(releaseBody);
    const release = await fetchRelease(latestReleaseUrl(), impl as any);
    expect(release.version).toBe("9.9.9");
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(latestReleaseUrl());
    expect(calls[0].init.headers["User-Agent"]).toBe(USER_AGENT);
    expect(calls[0].init.headers["User-Agent"]).not.toContain(PACKAGE_VERSION);
    expect(calls[0].init.headers).not.toHaveProperty("Authorization");
  });

  it("checks at most once per 24 h and records failures with a cause", async () => {
    const dir = mkdtempSync(join(tmpdir(), "uc-pi-update-"));
    const file = join(dir, "update.json");
    try {
      const { impl, calls } = fakeFetch("upstream unavailable", 503);
      const failed = await runCheck({ ...DEFAULT_UPDATE_STATE }, file, impl as any);
      expect(failed.lastError).toContain("HTTP 503");
      expect(failed.checkedAt).toBeGreaterThan(0);
      expect(statSync(file).mode & 0o777).toBe(0o600);
      expect(checkDue(failed)).toBe(false);
      const cached = await runCheck(failed, file, impl as any);
      expect(cached.lastError).toBe(failed.lastError);
      expect(calls).toHaveLength(1);
      expect(loadUpdateState(file).lastError).toBe("HTTP 503 from release API");
      expect(checkReport(null, cached.lastError)).toContain("HTTP 503");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("shows one notice per version per day, skips, and hides it on a newer one", () => {
    const release = { version: "9.9.9", notes: "notes" };
    const state: UpdateState = { ...DEFAULT_UPDATE_STATE, release };
    const now = 1_700_000_000_000;
    expect(showable(state, release, PACKAGE_VERSION, now)).toBe(true);
    markShown(state, release.version, now);
    expect(showable(state, release, PACKAGE_VERSION, now + 1000)).toBe(false);
    expect(showable(state, release, PACKAGE_VERSION, now + CHECK_INTERVAL_MS)).toBe(true);
    state.skippedVersion = release.version;
    expect(showable(state, release, PACKAGE_VERSION, now + CHECK_INTERVAL_MS * 2)).toBe(false);
    expect(showable(state, { version: "9.9.10", notes: "" }, PACKAGE_VERSION, now)).toBe(true);
    const text = notice(release, PACKAGE_VERSION);
    expect(text).toContain("ultracompress 9.9.9 available (you have " + PACKAGE_VERSION + ")");
    expect(text).toContain("update --auto on");
    expect(text).toContain("update --skip");
    expect(text).not.toContain("notes"); // release notes are not part of the one-line notice
  });

  it("turns checks off in CI, headless/agent mode and on opt-out", () => {
    expect(hostAllowed({})).toBe(true);
    expect(hostAllowed({ CI: "1" })).toBe(false);
    expect(hostAllowed({ PI_HEADLESS: "1" })).toBe(false);
    expect(hostAllowed({ PI_PRINT_MODE: "1" })).toBe(false);
    expect(hostAllowed({ ULTRACOMPRESS_AGENT_MODE: "1" })).toBe(false);
    expect(hostAllowed({ ULTRACOMPRESS_NO_UPDATE_CHECK: "1" })).toBe(false);
  });

  it("defaults checks on and auto-install off, persisted in settings", () => {
    expect(DEFAULT_SETTINGS.updates).toEqual({ checkEnabled: true, autoInstall: false, autoAsked: false });
    expect(mergeSettings({ updates: { autoInstall: true } }).updates).toEqual({
      checkEnabled: true, autoInstall: true, autoAsked: false,
    });
  });

  it("report-only check does not print JSON or release notes", () => {
    const report = checkReport({ version: "9.9.9", notes: "secret notes" }, null, PACKAGE_VERSION);
    expect(report).toContain("9.9.9 available");
    expect(report).not.toContain("secret notes");
    expect(report.startsWith("{")).toBe(false);
  });
});
