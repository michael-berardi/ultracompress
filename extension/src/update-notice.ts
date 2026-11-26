import { execFile } from "node:child_process";

/** Footer key: Pi collects ctx.ui.setStatus() texts into its status bar. */
export const UPDATE_STATUS_KEY = "ultracompress-update";
/** How often a long session asks the CLI again; the CLI itself checks the network at most daily. */
export const NOTICE_RECHECK_MS = 60 * 60 * 1000;

export interface CliResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

/** Run the CLI with piped stdio: it never sees a terminal, so it never prints its own notice. */
export function runCli(bin: string, args: string[], timeoutMs = 5000): Promise<CliResult> {
  return new Promise((resolve) => {
    execFile(bin, args, { timeout: timeoutMs, maxBuffer: 64 * 1024, windowsHide: true }, (error, stdout, stderr) => {
      const code = error ? (typeof error.code === "number" ? error.code : null) : 0;
      resolve({ code, stdout: String(stdout), stderr: String(stderr) || (error && code === null ? error.message : "") });
    });
  });
}

/**
 * Footer text from `ultracompress update --notice`. The CLI applies every rule
 * (opt-outs, CI, skip, once per version per day, opted-in installs); `{}` or
 * anything unreadable means no notice.
 */
export function footerText(stdout: string): string | undefined {
  try {
    const notice = JSON.parse(stdout) as { latest?: unknown; current?: unknown };
    if (typeof notice.latest === "string" && typeof notice.current === "string") {
      return `ultracompress ${notice.latest} available (you have ${notice.current}) · run: /ultracompress-update`;
    }
  } catch {}
  return undefined;
}

/** `/ultracompress-update [check|skip|auto on|off|status]` → CLI arguments; no argument installs. */
export function updateArgs(input: string): string[] | undefined {
  const [action = "install", value = "status"] = input.trim().split(/\s+/).filter(Boolean);
  if (action === "install") return ["update", "--yes"];
  if (action === "check") return ["update", "--check"];
  if (action === "skip") return ["update", "--skip"];
  if (action === "auto" && ["on", "off", "status"].includes(value)) return ["update", "--auto", value];
  return undefined;
}
