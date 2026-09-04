import { spawn } from "node:child_process";

export interface RcResult<T> {
  ok: boolean;
  data?: T;
  error?: string;
}

/** Spawn the rc binary with JSON on stdin; parse JSON from stdout. */
export function runRc<T>(bin: string, args: string[], stdinJson: unknown, timeoutMs = 10_000): Promise<RcResult<T>> {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(bin, args, { stdio: ["pipe", "pipe", "pipe"] });
    } catch (err) {
      resolve({ ok: false, error: `spawn failed: ${String(err)}` });
      return;
    }
    let stdout = "";
    let stderr = "";
    let settled = false;
    const finish = (r: RcResult<T>) => {
      if (!settled) {
        settled = true;
        resolve(r);
      }
    };
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      finish({ ok: false, error: `rc timed out after ${timeoutMs}ms` });
    }, timeoutMs);

    child.stdout.on("data", (d: Buffer) => (stdout += d.toString()));
    child.stderr.on("data", (d: Buffer) => (stderr += d.toString()));
    child.on("error", (err) => {
      clearTimeout(timer);
      finish({ ok: false, error: err.message });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      if (code !== 0) {
        finish({ ok: false, error: `rc exited ${code}: ${stderr.trim().slice(0, 300)}` });
        return;
      }
      try {
        finish({ ok: true, data: JSON.parse(stdout) as T });
      } catch (err) {
        finish({ ok: false, error: `bad rc output: ${String(err)}` });
      }
    });

    try {
      child.stdin.end(JSON.stringify(stdinJson));
    } catch (err) {
      clearTimeout(timer);
      finish({ ok: false, error: `stdin write failed: ${String(err)}` });
    }
  });
}
