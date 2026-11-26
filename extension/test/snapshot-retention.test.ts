import "./ultracompress-settings-mock.ts";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ultraCompressExtension from "../index.ts";
import { runUltraCompress } from "../src/bridge.ts";
import { loadSettings, mergeSettings } from "../src/settings.ts";
import { buildSnap, countSnaps, listSnaps, pruneSnaps, snapFileName, writeSnap, writeSnapEntries } from "../src/snapshot.ts";

vi.mock("../src/bridge.ts", () => ({ runUltraCompress: vi.fn() }));
// Native ESM namespaces are read-only; expose a mock namespace for fault injection.
vi.mock("node:fs", async (importOriginal) => ({ ...await importOriginal<typeof import("node:fs")>() }));
const DAY = 86_400_000;
const NOW = Date.UTC(2026, 9, 24, 12);
let cwd: string;
let dir: string;
const snapshots = () => fs.readdirSync(dir).filter((file) => /^snap-.*\.json$/.test(file));
function seed(timestamp: number, content = "synthetic conversation") {
  const file = snapFileName(timestamp);
  fs.writeFileSync(path.join(dir, file), content);
  return file;
}
beforeEach(() => {
  vi.clearAllMocks();
  cwd = fs.mkdtempSync(path.join(os.tmpdir(), "uc-retention-"));
  dir = path.join(cwd, ".steak-pi", "snaps");
  fs.mkdirSync(dir, { recursive: true });
});
afterEach(() => {
  vi.restoreAllMocks();
  fs.rmSync(cwd, { recursive: true, force: true });
});

describe("synthetic snapshot retention", () => {
  it("keeps the newest ten after a streamed write and removes every file older than fourteen days", () => {
    for (let i = 1; i <= 20; i++) seed(NOW - i * 1000);
    const expired = seed(NOW - 15 * DAY);
    const written = writeSnapEntries(cwd, [{ text: "synthetic current state" }], "threshold", NOW);
    expect(snapshots().sort()).toEqual(Array.from({ length: 10 }, (_, i) => snapFileName(NOW - i * 1000)).sort());
    expect(fs.existsSync(path.join(dir, expired))).toBe(false);
    expect(JSON.parse(fs.readFileSync(path.join(dir, written.file), "utf8")).entries).toEqual([{ text: "synthetic current state" }]);
    expect(listSnaps(cwd)[0]).toEqual(written);
  });

  it("honors custom limits, including the exact age boundary, on both writers", () => {
    seed(NOW - 2 * DAY - 1);
    const boundary = seed(NOW - 2 * DAY);
    writeSnapEntries(cwd, [], "test", NOW, { maxCount: 4, maxAgeDays: 2 });
    expect(snapshots().sort()).toEqual([boundary, snapFileName(NOW)].sort());
    const next = buildSnap([], "legacy writer", NOW + 1);
    writeSnap(cwd, next.payload, next.meta.file, { maxCount: 1, maxAgeDays: 2 }, NOW + 1);
    expect(snapshots()).toEqual([next.meta.file]);
  });

  it("does not parse payloads, trust metadata paths, follow symlinks, or prune unrelated files/cwds", () => {
    const other = path.join(cwd, "other", ".steak-pi", "snaps");
    fs.mkdirSync(other, { recursive: true });
    const oldName = snapFileName(NOW - 20 * DAY);
    const outside = path.join(other, oldName);
    fs.writeFileSync(outside, "preserve synthetic data");
    seed(NOW - 30 * DAY, JSON.stringify({ snap: { file: outside, createdAt: NOW } }));
    const linkName = snapFileName(NOW - 25 * DAY);
    fs.symlinkSync(outside, path.join(dir, linkName));
    const directoryName = snapFileName(NOW - 24 * DAY);
    fs.mkdirSync(path.join(dir, directoryName));
    for (const file of ["notes.json", "snap-invalid.json", "snap-2026-02-31T00-00-00-000Z.json", "snapshot.json"]) {
      fs.writeFileSync(path.join(dir, file), "preserve");
    }
    const read = vi.spyOn(fs, "readFileSync");
    expect(pruneSnaps(cwd, NOW)).toEqual([snapFileName(NOW - 30 * DAY)]);
    expect(read).not.toHaveBeenCalled();
    expect(fs.lstatSync(path.join(dir, linkName)).isSymbolicLink()).toBe(true);
    expect(fs.statSync(path.join(dir, directoryName)).isDirectory()).toBe(true);
    expect(fs.readFileSync(outside, "utf8")).toBe("preserve synthetic data");
    expect(fs.readdirSync(dir)).toHaveLength(6);
  });

  it("refuses redirected directories without deleting their contents", () => {
    fs.rmdirSync(dir);
    const outside = path.join(cwd, "redirected");
    fs.mkdirSync(outside);
    fs.writeFileSync(path.join(outside, snapFileName(NOW - 20 * DAY)), "preserve");
    fs.symlinkSync(outside, dir);
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(pruneSnaps(cwd, NOW)).toEqual([]);
    expect(log).toHaveBeenCalledWith("[ultracompress] snapshot retention failed:", expect.any(Error));
    expect(fs.readdirSync(outside)).toHaveLength(1);
  });

  it("does not prune before a successful write", () => {
    const previous = seed(NOW - 20 * DAY);
    expect(() => writeSnapEntries(cwd, [BigInt(1)], "bad entry", NOW)).toThrow();
    expect(snapshots()).toEqual([previous]);
  });

  it("logs deletion failures, preserves the new snapshot and continues pruning", () => {
    const blocked = seed(NOW - 30 * DAY);
    const deletable = seed(NOW - 20 * DAY);
    const unlink = fs.unlinkSync;
    vi.spyOn(fs, "unlinkSync").mockImplementation((file) => {
      if (String(file).endsWith(blocked)) throw Object.assign(new Error("synthetic permission denied"), { code: "EACCES" });
      return unlink(file);
    });
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const current = writeSnapEntries(cwd, [], "test", NOW);
    expect(snapshots().sort()).toEqual([blocked, current.file].sort());
    expect(fs.existsSync(path.join(dir, deletable))).toBe(false);
    expect(log).toHaveBeenCalledWith(expect.stringContaining(blocked), expect.objectContaining({ code: "EACCES" }));
  });

  it("writes owner-only snapshots and tightens loose pre-existing directories", () => {
    fs.chmodSync(path.join(cwd, ".steak-pi"), 0o777);
    fs.chmodSync(dir, 0o777);
    const streamed = writeSnapEntries(cwd, [{ text: "synthetic private state" }], "threshold", NOW);
    const legacy = buildSnap([], "legacy writer", NOW + 1);
    const legacyFile = writeSnap(cwd, legacy.payload, legacy.meta.file, undefined, NOW + 1);
    expect(fs.statSync(path.join(cwd, ".steak-pi")).mode & 0o777).toBe(0o700);
    expect(fs.statSync(dir).mode & 0o777).toBe(0o700);
    expect(fs.statSync(path.join(dir, streamed.file)).mode & 0o777).toBe(0o600);
    expect(fs.statSync(legacyFile).mode & 0o777).toBe(0o600);
  });

  it("drops the oldest snapshots first once the size budget is exceeded", () => {
    const small = seed(NOW - 3000, "x".repeat(100));
    const large = seed(NOW - 2000, "x".repeat(4000));
    const newer = seed(NOW - 1000, "x".repeat(4000));
    // The budget fits the two newest; everything older goes, even a small file that would fit.
    expect(pruneSnaps(cwd, NOW, undefined, 8050)).toEqual([small]);
    expect(snapshots().sort()).toEqual([large, newer].sort());
    expect(pruneSnaps(cwd, NOW, undefined, 4000)).toEqual([large]);
    expect(snapshots()).toEqual([newer]);
  });

  it("keeps the newest snapshot even when it alone exceeds the size budget", () => {
    const older = seed(NOW - 2000, "x".repeat(10));
    const newest = seed(NOW - 1000, "x".repeat(5000));
    expect(pruneSnaps(cwd, NOW, undefined, 100)).toEqual([older]);
    expect(snapshots()).toEqual([newest]);
  });

  it("counts retained snapshots from directory entries without reading contents", () => {
    for (let i = 1; i <= 3; i++) seed(NOW - i * 1000);
    fs.writeFileSync(path.join(dir, "notes.json"), "ignore");
    fs.mkdirSync(path.join(dir, snapFileName(NOW - 5000)));
    const read = vi.spyOn(fs, "readFileSync");
    expect(countSnaps(cwd)).toBe(3);
    expect(countSnaps(path.join(cwd, "missing"))).toBe(0);
    expect(read).not.toHaveBeenCalled();
  });

  it("defaults old settings and rejects unbounded or invalid retention values", () => {
    expect(mergeSettings({ snapshot: { enabled: false } }).snapshot).toEqual({ enabled: false, maxCount: 10, maxAgeDays: 14 });
    for (const value of [0, -1, NaN, Infinity, "all", null]) {
      expect(mergeSettings({ snapshot: { maxCount: value, maxAgeDays: value } }).snapshot).toEqual({ enabled: true, maxCount: 10, maxAgeDays: 14 });
    }
    expect(mergeSettings({ snapshot: { maxCount: 3.8, maxAgeDays: 2.9 } }).snapshot).toEqual({ enabled: true, maxCount: 3, maxAgeDays: 2 });
    for (let i = 1; i < 20; i++) seed(NOW - i);
    writeSnapEntries(cwd, [], "invalid caller", NOW, { maxCount: NaN, maxAgeDays: Infinity });
    expect(snapshots()).toHaveLength(10);
  });

  it("passes configured retention through the actual pre-compaction hook", async () => {
    const settings = mergeSettings({ snapshot: { maxCount: 2, maxAgeDays: 1 } });
    vi.mocked(loadSettings).mockReturnValue(settings);
    vi.spyOn(process, "cwd").mockReturnValue(cwd);
    vi.spyOn(Date, "now").mockReturnValue(NOW);
    seed(NOW - 2 * DAY);
    seed(NOW - 2000);
    seed(NOW - 1000);
    const handlers = new Map<string, any>();
    ultraCompressExtension({ on(name: string, fn: any) { handlers.set(name, fn); }, registerTool() {}, registerCommand() {} } as never);
    vi.mocked(runUltraCompress).mockResolvedValue({ ok: false, error: "synthetic bridge unavailable" });
    await handlers.get("session_before_compact")({ preparation: { tokensBefore: 100 }, branchEntries: [] }, { sessionManager: { getEntries: () => [] } });
    expect(snapshots().sort()).toEqual([snapFileName(NOW - 1000), snapFileName(NOW)].sort());
  });
});
