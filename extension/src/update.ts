/**
 * Pi adapter side of the 2026-11-26 update contract.
 *
 * A session-start notice is the agent-host equivalent of the CLI's post-command
 * stderr notice. One anonymous GET, at most daily, only when a live UI exists;
 * never in CI, headless/print mode, or when the user opted out. Auto-install is
 * opt-in and is delegated to the CLI, which owns checksum verification.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { PACKAGE_VERSION, REPOSITORY } from "./metadata.ts";

export const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;
export const CHECK_TIMEOUT_MS = 4000;
export const USER_AGENT = "ultracompress-update-check";
export const OPT_OUT_ENV = "ULTRACOMPRESS_NO_UPDATE_CHECK";

export interface Release {
  version: string;
  notes: string;
}

export interface UpdateState {
  checkedAt: number;
  lastError: string | null;
  release: Release | null;
  skippedVersion: string | null;
  shownVersion: string | null;
  shownAt: number;
  autoAttemptedVersion: string | null;
}

export const DEFAULT_UPDATE_STATE: UpdateState = {
  checkedAt: 0,
  lastError: null,
  release: null,
  skippedVersion: null,
  shownVersion: null,
  shownAt: 0,
  autoAttemptedVersion: null,
};

export function updateCachePath(): string {
  const root = process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
  return path.join(root, "ultracompress", "update.json");
}

function privateDir(dir: string): void {
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  try { fs.chmodSync(dir, 0o700); } catch {}
}

export function loadUpdateState(file = updateCachePath()): UpdateState {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    if (parsed && typeof parsed === "object") return { ...DEFAULT_UPDATE_STATE, ...parsed };
  } catch {}
  return { ...DEFAULT_UPDATE_STATE };
}

export function saveUpdateState(state: UpdateState, file = updateCachePath()): void {
  privateDir(path.dirname(file));
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(state, null, 2) + "\n", { mode: 0o600 });
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

export function checkDue(state: UpdateState, now = Date.now()): boolean {
  return state.checkedAt <= 0 || now - state.checkedAt >= CHECK_INTERVAL_MS;
}

export function showable(state: UpdateState, release: Release, current = PACKAGE_VERSION, now = Date.now()): boolean {
  if (!isNewer(release.version, current)) return false;
  if (state.skippedVersion === release.version) return false;
  if (state.shownVersion === release.version && now - state.shownAt < CHECK_INTERVAL_MS) return false;
  return true;
}

export function markShown(state: UpdateState, version: string, now = Date.now()): void {
  state.shownVersion = version;
  state.shownAt = now;
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
  const now = Date.now();
  if (!checkDue(next, now)) return next;
  try {
    next.release = await fetchRelease(latestReleaseUrl(), fetchImpl);
    next.lastError = null;
  } catch (error) {
    next.lastError = error instanceof Error ? error.message : String(error);
  }
  next.checkedAt = now;
  try { saveUpdateState(next, file); } catch {}
  return next;
}
