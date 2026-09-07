import { spawn, type ChildProcess } from "node:child_process";
import { StringDecoder } from "node:string_decoder";

export interface UltraCompressResult<T> {
  ok: boolean;
  data?: T;
  error?: string;
}

/** Output caps keep the same failure class (oversized strings) from surfacing as parse-side RangeErrors. */
const MAX_STDOUT_BYTES = 64 * 1024 * 1024;
const MAX_STDERR_BYTES = 1024 * 1024;

/** Thrown when the writer stops because the child settled or exited; callers treat it as a no-op race loss. */
class WriterCancelled extends Error {}

const NO_VALUE = Symbol("uc-no-value");

type Write = (chunk: string) => Promise<void>;

const WRAPPER_PROTOS = new Map<object, "number" | "string" | "boolean" | "bigint">([
  [Number.prototype, "number"],
  [String.prototype, "string"],
  [Boolean.prototype, "boolean"],
  [BigInt.prototype, "bigint"],
]);

/**
 * Internal-wrapper detection by prototype identity, reading the wrapper's
 * internal value through the matched prototype's OWN valueOf. Spoof-proof
 * against Symbol.toStringTag, subclass-aware (class X extends Number {}),
 * and slotless inheritors (Object.create(Number.prototype)) fall back to
 * plain-object serialization exactly like native JSON.
 */
function wrapperSlot(value: object): { kind: "number" | "string" | "boolean" | "bigint"; slot: unknown } | null {
  let proto: object | null = Object.getPrototypeOf(value);
  while (proto !== null) {
    const kind = WRAPPER_PROTOS.get(proto);
    if (kind) {
      try {
        const valueOf = Object.getOwnPropertyDescriptor(proto, "valueOf")?.value as
          | ((this: unknown) => unknown)
          | undefined;
        const slot = valueOf?.call(value);
        if (typeof slot === kind) return { kind, slot };
      } catch {
        return null; // slotless inheritor: native treats it as a plain object
      }
      return null;
    }
    proto = Object.getPrototypeOf(proto);
  }
  return null;
}

/**
 * ToPrimitive with an explicit hint: an exotic Symbol.toPrimitive method
 * wins (and must return a primitive), otherwise ordinary valueOf-first for
 * the number hint and toString-first for the string hint.
 */
function toPrimitiveHinted(value: object, hint: "number" | "string"): unknown {
  const exotic = (value as Record<PropertyKey, unknown>)[Symbol.toPrimitive];
  if (typeof exotic === "function") {
    const result = (exotic as (hint: string) => unknown).call(value, hint);
    if (result !== null && (typeof result === "object" || typeof result === "function")) {
      throw new TypeError("Cannot convert object to primitive value");
    }
    return result;
  }
  const methods = hint === "number" ? (["valueOf", "toString"] as const) : (["toString", "valueOf"] as const);
  for (const method of methods) {
    const fn = (value as Record<string, unknown>)[method];
    if (typeof fn === "function") {
      const result = (fn as (this: unknown) => unknown).call(value);
      if (result !== null && (typeof result === "object" || typeof result === "function")) continue;
      return result;
    }
  }
  throw new TypeError("Cannot convert object to primitive value");
}

/**
 * Serialize one value the way JSON.stringify would, emitting it leaf by leaf
 * so no payload-sized string is ever materialized (V8 max string length is
 * ~512 MiB chars; whole-session payloads cross that and throw
 * `RangeError: Invalid string length`). Follows the JSON.stringify
 * serialization algorithm: toJSON runs exactly once with the property key
 * (its result is serialized without re-checking toJSON), boxed
 * Number/String/Boolean unbox, boxed BigInt throws, holes and
 * undefined/function/symbol array elements become null, such property
 * values — and values that collapse to undefined via toJSON — are omitted
 * from objects, bigints are rejected loudly like native JSON.
 * Returns NO_VALUE where JSON.stringify would produce undefined; in that
 * case nothing has been written.
 */
async function serialize(v: unknown, key: string, write: Write): Promise<void | typeof NO_VALUE> {
  let value = v;
  if ((typeof value === "object" || typeof value === "function") && value !== null) {
    const toJSON = (value as { toJSON?: unknown }).toJSON;
    if (typeof toJSON === "function") {
      value = (toJSON as (k: string) => unknown).call(value, key);
    }
  }
  return serializeValue(value, key, write);
}

async function serializeValue(value: unknown, key: string, write: Write): Promise<void | typeof NO_VALUE> {
  if (value === null) {
    await write("null");
    return;
  }
  const type = typeof value;
  if (type === "string") {
    await write(JSON.stringify(value));
    return;
  }
  if (type === "number") {
    await write(Number.isFinite(value as number) ? String(value) : "null");
    return;
  }
  if (type === "boolean") {
    await write(value ? "true" : "false");
    return;
  }
  if (type === "bigint") {
    throw new TypeError("Do not know how to serialize a BigInt");
  }
  if (type === "object") {
    const wrapper = wrapperSlot(value as object);
    if (wrapper?.kind === "bigint") {
      throw new TypeError("Do not know how to serialize a BigInt");
    }
    if (wrapper?.kind === "boolean") {
      // Native reads the Boolean internal slot directly: neither a shadowing
      // valueOf/toString nor an exotic Symbol.toPrimitive participates.
      await write(wrapper.slot ? "true" : "false");
      return;
    }
    if (wrapper?.kind === "number" || wrapper?.kind === "string") {
      // Native wrapper semantics: Number wrappers ToPrimitive with the
      // number hint then ToNumber the result (a shadowing valueOf wins;
      // undefined → NaN → "null", null → 0, true → 1, "s" → NaN → "null");
      // String wrappers ToPrimitive with the string hint (toString first)
      // then ToString the result and quote it (null → "null", 42 → "42").
      const hint = wrapper.kind === "number" ? "number" : "string";
      const prim = toPrimitiveHinted(value as object, hint);
      if (typeof prim === "bigint") {
        throw new TypeError(
          hint === "number" ? "Cannot convert a BigInt value to a number" : "Do not know how to serialize a BigInt",
        );
      }
      if (hint === "number") {
        const num = Number(prim);
        await write(Number.isFinite(num) ? String(num) : "null");
      } else {
        await write(JSON.stringify(String(prim)));
      }
      return;
    }
    if (Array.isArray(value)) {
      // Native captures the length before the walk; toJSON hooks that shrink
      // the array still produce null placeholders for the dropped elements.
      const len = (value as unknown[]).length;
      await write("[");
      for (let i = 0; i < len; i++) {
        if (i > 0) await write(",");
        const item = (value as unknown[])[i];
        if ((await serialize(item, String(i), write)) === NO_VALUE) await write("null");
      }
      await write("]");
      return;
    }
    await write("{");
    // Key and comma are deferred until the value's first emitted chunk, so a
    // property whose value serializes to undefined is omitted entirely and
    // comma placement stays native-exact.
    let needComma = false;
    for (const prop of Object.keys(value as Record<string, unknown>)) {
      const val = (value as Record<string, unknown>)[prop];
      const prefix = `${needComma ? "," : ""}${JSON.stringify(prop)}:`;
      let prefixWritten = false;
      const keyedWrite: Write = async (chunk) => {
        if (!prefixWritten) {
          prefixWritten = true;
          await write(prefix);
        }
        await write(chunk);
      };
      const result = await serialize(val, prop, keyedWrite);
      if (result === NO_VALUE && !prefixWritten) continue; // property omitted; comma state unchanged
      needComma = true;
      if (result === NO_VALUE) await write("null"); // unreachable: NO_VALUE implies nothing written
    }
    await write("}");
    return;
  }
  // Root-level undefined, function, or symbol: JSON.stringify yields undefined.
  return NO_VALUE;
}

/** Race a pending drain against child exit, errors, and cancellation. */
function waitDrain(stdin: NodeJS.WritableStream, isCancelled: () => boolean): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    let done = false;
    const finish = (err?: Error): void => {
      if (done) return;
      done = true;
      stdin.removeListener("drain", onDrain);
      stdin.removeListener("error", onError);
      stdin.removeListener("close", onClose);
      clearInterval(poll);
      if (err) reject(err);
      else resolve();
    };
    const onDrain = (): void => finish();
    const onError = (err: Error): void => finish(err);
    const onClose = (): void => finish(new WriterCancelled("child closed before stdin drained"));
    const poll = setInterval(() => {
      if (isCancelled()) finish(new WriterCancelled("writer cancelled"));
    }, 25);
    stdin.on("drain", onDrain);
    stdin.on("error", onError);
    stdin.on("close", onClose);
  });
}

/**
 * Write a JSON value to the child stdin incrementally, awaiting drain when
 * the pipe backs up. A single JSON.stringify of the whole payload breaks
 * once it crosses V8's max string length (~512 MiB chars); per-leaf strings
 * stay far below that for UC payloads (one escaped string beyond the limit
 * fails exactly as the old whole-payload stringify did). Resolves after
 * `end()`; rejects on serialization or write failure. Cancels via
 * `isCancelled` once the caller has settled by timeout or child exit.
 */
export async function streamJsonTo(child: ChildProcess, value: unknown, isCancelled: () => boolean = () => false): Promise<void> {
  const stdin = child.stdin;
  if (!stdin) throw new Error("child stdin unavailable");
  const write: Write = async (chunk) => {
    if (isCancelled()) throw new WriterCancelled("writer cancelled");
    if (!stdin.write(chunk)) await waitDrain(stdin, isCancelled);
  };
  await serialize(value, "", write);
  stdin.end();
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
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let stdinError: Error | undefined;
    let childClosed = false;
    let settled = false;
    const stdoutDecoder = new StringDecoder("utf8");
    const stderrDecoder = new StringDecoder("utf8");
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

    // A child that exits early must not surface EPIPE as an unhandled
    // 'error' event while payloads are fed incrementally; the close handler
    // folds a stdin failure into the result instead.
    child.stdin?.on("error", (err: Error) => {
      stdinError = err;
    });
    child.stdout.on("data", (d: Buffer) => {
      stdoutBytes += d.length;
      if (stdoutBytes > MAX_STDOUT_BYTES) {
        child.kill("SIGKILL");
        finish({ ok: false, error: `UltraCompress stdout exceeded ${MAX_STDOUT_BYTES} byte cap` });
        return;
      }
      // StringDecoder keeps multi-byte characters intact across chunk splits.
      stdout += stdoutDecoder.write(d);
    });
    child.stderr.on("data", (d: Buffer) => {
      stderrBytes += d.length;
      if (stderrBytes > MAX_STDERR_BYTES) return;
      stderr += stderrDecoder.write(d);
    });
    child.on("error", (err) => {
      clearTimeout(timer);
      finish({ ok: false, error: err.message });
    });
    child.on("close", (code) => {
      childClosed = true;
      clearTimeout(timer);
      try {
        // Flush any bytes a trailing partial multi-byte sequence held back.
        stdout += stdoutDecoder.end();
        stderr += stderrDecoder.end();
      } catch {}
      if (code !== 0) {
        finish({ ok: false, error: `UltraCompress exited ${code}: ${stderr.trim().slice(0, 300)}` });
        return;
      }
      if (stdinError) {
        finish({ ok: false, error: `stdin write failed: ${stdinError.message}` });
        return;
      }
      try {
        finish({ ok: true, data: JSON.parse(stdout) as T });
      } catch (err) {
        finish({ ok: false, error: `bad UltraCompress output: ${String(err)}` });
      }
    });

    const isCancelled = (): boolean => settled || childClosed;
    streamJsonTo(child, stdinJson, isCancelled).catch((err) => {
      if (err instanceof WriterCancelled) return; // timeout/close path owns the result
      clearTimeout(timer);
      child.kill("SIGKILL");
      finish({ ok: false, error: `stdin write failed: ${String(err)}` });
    });
  });
}
