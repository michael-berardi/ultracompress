/**
 * Pi adapter side of the 2026-11-26 update contract.
 *
 * A session-start notice is the agent-host equivalent of the CLI's post-command
 * stderr notice. One anonymous GET, at most daily, only when a live UI exists;
 * never in CI, headless/print mode, or when the user opted out. Auto-install is
 * opt-in and is delegated to the CLI, which owns checksum verification.
 *
 * The cache is shared with the CLI and Hermes: one `update.json` in the
 * platform cache directory, snake_case keys and integer Unix seconds. A
 * pre-0.6.8 Pi cache (camelCase keys, milliseconds) is migrated on read.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { PACKAGE_VERSION, REPOSITORY } from "./metadata.ts";

/** One check per 24 h per machine, and one notice per 24 h per version. */
export const CHECK_INTERVAL_SECS = 24 * 60 * 60;
export const CHECK_TIMEOUT_MS = 4000;
export const CACHE_FILE = "update.json";
export const USER_AGENT = "ultracompress-update-check";
export const OPT_OUT_ENV = "ULTRACOMPRESS_NO_UPDATE_CHECK";

export interface Release {
  version: string;
  notes: string;
}

export interface UpdateState {
  checked_at: number;
  last_error: string | null;
  release: Release | null;
  skipped_version: string | null;
  shown_version: string | null;
  shown_at: number;
  auto_attempted_version: string | null;
}

export const DEFAULT_UPDATE_STATE: UpdateState = {
  checked_at: 0,
  last_error: null,
  release: null,
  skipped_version: null,
  shown_version: null,
  shown_at: 0,
  auto_attempted_version: null,
};

/** Same cache root as Rust and Python: XDG, Windows LocalAppData, macOS
 * Library/Caches, then ~/.cache. The file name and time unit are shared too. */
export function updateCachePath(
  env: NodeJS.ProcessEnv = process.env,
  platform: NodeJS.Platform = process.platform,
  home: string = os.homedir(),
): string {
  if (env.XDG_CACHE_HOME) return path.join(env.XDG_CACHE_HOME, "ultracompress", CACHE_FILE);
  if (platform === "win32" && env.LOCALAPPDATA) {
    return path.join(env.LOCALAPPDATA, "ultracompress", CACHE_FILE);
  }
  if (platform === "darwin" && home) {
    return path.join(home, "Library", "Caches", "ultracompress", CACHE_FILE);
  }
  if (home) return path.join(home, ".cache", "ultracompress", CACHE_FILE);
  return path.join(process.cwd(), "ultracompress-update-cache", CACHE_FILE);
}

function privateDir(dir: string): void {
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  try { fs.chmodSync(dir, 0o700); } catch {}
}

function toSeconds(value: unknown, milliseconds: boolean): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) return 0;
  return Math.floor(milliseconds ? value / 1000 : value);
}

function asText(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function asRelease(value: unknown): Release | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  if (typeof record.version !== "string" || typeof record.notes !== "string") return null;
  return { version: record.version, notes: record.notes };
}

/** Read the canonical schema, migrating the old Pi camelCase millisecond file. */
export function parseUpdateState(body: string): UpdateState {
  let parsed: unknown;
  try { parsed = JSON.parse(body); } catch { return { ...DEFAULT_UPDATE_STATE }; }
  if (!parsed || typeof parsed !== "object") return { ...DEFAULT_UPDATE_STATE };
  const record = parsed as Record<string, unknown>;
  const legacyChecked = record.checkedAt;
  const legacyShown = record.shownAt;
  return {
    checked_at: toSeconds(record.checked_at ?? legacyChecked, legacyChecked !== undefined),
    last_error: asText(record.last_error),
    release: asRelease(record.release),
    skipped_version: asText(record.skipped_version ?? record.skippedVersion),
    shown_version: asText(record.shown_version ?? record.shownVersion),
    shown_at: toSeconds(record.shown_at ?? legacyShown, legacyShown !== undefined),
    auto_attempted_version: asText(record.auto_attempted_version ?? record.autoAttemptedVersion),
  };
}

export function loadUpdateState(file = updateCachePath()): UpdateState {
  try {
    return parseUpdateState(fs.readFileSync(file, "utf8"));
  } catch {
    return { ...DEFAULT_UPDATE_STATE };
  }
}

/** Write exactly one canonical state object: snake_case keys, Unix seconds. */
export function saveUpdateState(state: UpdateState, file = updateCachePath()): void {
  privateDir(path.dirname(file));
  const canonical: UpdateState = {
    checked_at: Math.floor(state.checked_at > 0 ? state.checked_at : 0),
    last_error: state.last_error,
    release: state.release && {
      version: state.release.version,
      notes: state.release.notes,
    },
    skipped_version: state.skipped_version,
    shown_version: state.shown_version,
    shown_at: Math.floor(state.shown_at > 0 ? state.shown_at : 0),
    auto_attempted_version: state.auto_attempted_version,
  };
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(canonical, null, 2) + "\n", { mode: 0o600 });
  try { fs.chmodSync(tmp, 0o600); } catch {}
  fs.renameSync(tmp, file);
}

export function parseVersion(text: string): [number, number, number] | null {
  const body = text.startsWith("v") ? text.slice(1) : text;
  if (!/^\d+\.\d+\.\d+$/.test(body)) return null;
  const parts = body.split(".").map(Number);
  return [parts[0], parts[1], parts[2]];
}

export function isNewer(latest: string, current: string): boolean {
  const a = parseVersion(latest);
  const b = parseVersion(current);
  if (!a || !b) return false;
  return a[0] !== b[0] ? a[0] > b[0] : a[1] !== b[1] ? a[1] > b[1] : a[2] > b[2];
}

/** Only a stable x.y.z GitHub release; drafts and prereleases are ignored. */
export function parseRelease(body: string): Release {
  const value = JSON.parse(body) as Record<string, unknown>;
  if (value.draft === true) throw new Error("latest release is a draft; ignored");
  if (value.prerelease === true) throw new Error("latest release is a prerelease; ignored");
  const tag = typeof value.tag_name === "string" ? value.tag_name : "";
  if (!parseVersion(tag)) throw new Error(`release tag '${tag}' is not a stable x.y.z version`);
  const notes = typeof value.body === "string"
    ? value.body.split(/\r?\n/).filter((line) => line.trim()).slice(0, 5).join("\n").slice(0, 600)
    : "";
  return { version: (tag.startsWith("v") ? tag.slice(1) : tag), notes };
}

/** Owner/repo comes from the package's own metadata, never a hard-coded person. */
export function latestReleaseUrl(repository = REPOSITORY): string {
  const match = /^https:\/\/github\.com\/([^/]+)\/([^/]+?)(?:\.git)?\/?$/.exec(repository);
  if (!match) throw new Error(`repository metadata is not a GitHub URL: ${repository}`);
  return `https://api.github.com/repos/${match[1]}/${match[2]}/releases/latest`;
}

export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

export function checkDue(state: UpdateState, now = nowSeconds()): boolean {
  return state.checked_at <= 0 || now - state.checked_at >= CHECK_INTERVAL_SECS;
}

export function showable(state: UpdateState, release: Release, current = PACKAGE_VERSION, now = nowSeconds()): boolean {
  if (!isNewer(release.version, current)) return false;
  if (state.skipped_version === release.version) return false;
  if (state.shown_version === release.version && now - state.shown_at < CHECK_INTERVAL_SECS) return false;
  return true;
}

export function markShown(state: UpdateState, version: string, now = nowSeconds()): void {
  state.shown_version = version;
  state.shown_at = now;
}

export function notice(release: Release, current = PACKAGE_VERSION): string {
  return `ultracompress ${release.version} available (you have ${current}) · run: ultracompress update\n` +
    `turn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip`;
}

export function checkReport(release: Release | null, error: string | null, current = PACKAGE_VERSION): string {
  if (release && isNewer(release.version, current)) {
    return `ultracompress ${release.version} available (you have ${current}) · run: ultracompress update`;
  }
  if (error) return `ultracompress update check failed: ${error}`;
  return `ultracompress is up to date (${current})`;
}

/** True only in a live interactive host. CI/print/headless and opt-out are off. */
export function hostAllowed(env: NodeJS.ProcessEnv = process.env): boolean {
  if (env[OPT_OUT_ENV] === "1") return false;
  if (env.CI) return false;
  if (env.PI_HEADLESS === "1" || env.PI_PRINT_MODE === "1" || env.ULTRACOMPRESS_AGENT_MODE === "1") return false;
  return true;
}

type FetchLike = (url: string, init?: Record<string, unknown>) => Promise<{
  ok: boolean;
  status: number;
  text: () => Promise<string>;
}>;

/** The single anonymous GET; no credentials, no identifiers, four-second cap. */
export async function fetchRelease(url = latestReleaseUrl(), fetchImpl: FetchLike = fetch as unknown as FetchLike): Promise<Release> {
  const response = await fetchImpl(url, {
    method: "GET",
    headers: { "User-Agent": USER_AGENT, Accept: "application/vnd.github+json" },
    signal: AbortSignal.timeout(CHECK_TIMEOUT_MS),
  });
  if (!response.ok) throw new Error(`HTTP ${response.status} from release API`);
  return parseRelease(await response.text());
}

/** Check only when due; a failed check is recorded with its cause, never thrown. */
export async function runCheck(state: UpdateState, file = updateCachePath(), fetchImpl?: FetchLike): Promise<UpdateState> {
  const next: UpdateState = { ...state };
  if (!checkDue(next)) return next;
  try {
    next.release = await fetchRelease(latestReleaseUrl(), fetchImpl);
    next.last_error = null;
  } catch (error) {
    next.last_error = error instanceof Error ? error.message : String(error);
  }
  next.checked_at = nowSeconds();
  try { saveUpdateState(next, file); } catch {}
  return next;
}
