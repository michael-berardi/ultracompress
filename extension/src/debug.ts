import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

/**
 * Optional diagnostics (settings.debug). The file can contain a short summary
 * preview, so it lives in the user's private state directory, never /tmp, and
 * is overwritten on each event instead of growing a history.
 */
export function debugPath(): string {
  const base = process.env.XDG_STATE_HOME || path.join(os.homedir(), ".local", "state");
  return path.join(base, "ultracompress", "debug.json");
}

export function writeDebug(data: Record<string, unknown>): void {
  const file = debugPath();
  const dir = path.dirname(file);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  try {
    fs.chmodSync(dir, 0o700);
  } catch {}
  fs.writeFileSync(file, JSON.stringify(data, null, 2) + "\n", { mode: 0o600 });
  try {
    fs.chmodSync(file, 0o600);
  } catch {}
}
