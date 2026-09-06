import { spawn } from "node:child_process";

export interface UltraCompressResult<T> {
  ok: boolean;
  data?: T;
  error?: string;
}

/** Spawn the UltraCompress binary with JSON on stdin; parse JSON from stdout. */
export function runUltraCompress<T>(bin: string, args: string[], stdinJson: unknown, timeoutMs = 10_000): Promise<UltraCompressResult<T>> {
  return new Promise((resolve) => {
    let child;
    try {
      // Opt into the shared UC telemetry sink so agent-side savings are
      // counted in `uc telemetry` / `utp savings` (default state path).
      child = spawn(bin, args, {
        stdio: ["pipe", "pipe", "pipe"],
        env: {
          ...process.env,
          UC_TEXT_ENVELOPES: "1", // this extension can safely retrieve by reference
          ...(process.env.UC_TELEMETRY === undefined && process.env.UC_TELEMETRY_PATH === undefined
            ? { UC_TELEMETRY: "1" } : {}),
        },
      });
    } catch (err) {
      resolve({ ok: false, error: `spawn failed: ${String(err)}` });
      return;
    }
    let stdout = "";
    let stderr = "";
    let settled = false;
    const finish = (r: UltraCompressResult<T>) => {
      if (!settled) {
        settled = true;
        resolve(r);
      }
    };
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      finish({ ok: false, error: `UltraCompress timed out after ${timeoutMs}ms` });
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
        finish({ ok: false, error: `UltraCompress exited ${code}: ${stderr.trim().slice(0, 300)}` });
        return;
      }
      try {
        finish({ ok: true, data: JSON.parse(stdout) as T });
      } catch (err) {
        finish({ ok: false, error: `bad UltraCompress output: ${String(err)}` });
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
