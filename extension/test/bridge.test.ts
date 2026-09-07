import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { runUltraCompress, streamJsonTo } from "../src/bridge.ts";
import type { ChildProcess } from "node:child_process";

const V8_MAX_STRING_LENGTH = 536_870_888;

/** Writable double that records chunks without ever joining them into one string. */
class RecordingStdin {
  chunks: string[] = [];
  totalLength = 0;
  head = "";
  tail = "";
  ended = false;
  write(chunk: string): boolean {
    this.chunks.push(chunk);
    this.totalLength += chunk.length;
    this.head = (this.head + chunk).slice(0, 32);
    this.tail = (this.tail + chunk).slice(-32);
    return true; // never signals backpressure
  }
  end(): void {
    this.ended = true;
  }
  on(): void {}
  text(): string {
    return this.chunks.join("");
  }
}

const asChild = (stdin: RecordingStdin): ChildProcess => ({ stdin }) as unknown as ChildProcess;

describe("streamJsonTo", () => {
  it("produces byte-identical JSON to JSON.stringify for representative payloads", async () => {
    const payloads: unknown[] = [
      { a: 1, b: "x", c: null, d: true },
      { entries: [], policy: "snap" },
      { nested: { deep: [{ unicode: "α β 日本語 café \"quoted\"", empty: "", zero: 0 }] } },
      { omitted: undefined, kept: 1, arr: [undefined, 2, null] },
      { special: "line\nbreak\ttab\"quote/backslash\\" },
      { holes: [1, undefined, 3] },
      {},
      [],
    ];
    for (const payload of payloads) {
      const rec = new RecordingStdin();
      await streamJsonTo(asChild(rec), payload);
      expect(rec.text()).toBe(JSON.stringify(payload));
      expect(rec.ended).toBe(true);
    }
  });

  it("matches JSON.stringify toJSON semantics, including the property key", async () => {
    const cases: Array<[unknown, string]> = [
      [{ x: { toJSON(key: string) { return key; } } }, '{"x":"x"}'],
      [[{ toJSON(key: string) { return key; } }], '["0"]'],
      [{ toJSON() { return { a: 1 }; } }, '{"a":1}'],
      [Object.assign([], { toJSON() { return "replacement"; } }), '"replacement"'],
      [{ d: new Date(0) }, '{"d":"1970-01-01T00:00:00.000Z"}'],
      // toJSON runs exactly once; the replacement is not re-checked.
      [{ toJSON() { return this; }, a: 1 }, '{"a":1}'],
      [{ x: { toJSON() { return undefined; } }, y: 1 }, '{"y":1}'],
      [{ a: 1, x: { toJSON() { return undefined; } }, b: 2 }, '{"a":1,"b":2}'],
      [{ x: { toJSON() { return { toJSON() { return 42; } }; } } }, '{"x":{}}'],
      [[{ toJSON() { return undefined; } }], "[null]"],
      [{ a: { b: { toJSON(key: string) { return key; } } } }, '{"a":{"b":"b"}}'],
      // toJSON hook mutating the parent: key list is fixed, values read lazily.
      [{ a: { toJSON() { (this as { b?: unknown }).constructor; return 0; } }, b: 1 }, '{"a":0,"b":1}'],
      // Callable objects get their toJSON honored before omission rules.
      [{ x: Object.assign((): void => undefined, { toJSON() { return 7; } }) }, '{"x":7}'],
      [[Object.assign((): void => undefined, { toJSON() { return 7; } })], "[7]"],
      // Symbol.toStringTag cannot spoof internal-wrapper detection.
      [{ [Symbol.toStringTag]: "BigInt", x: 1 }, '{"x":1}'],
      // Wrapper subclasses unbox through the prototype chain like native.
      [{ n: new (class MyNumber extends Number {})(7) }, '{"n":7}'],
      // Exotic Symbol.toPrimitive participates on wrappers only.
      [{ n: Object.assign(new Number(7), { [Symbol.toPrimitive]() { return 42; } }) }, '{"n":42}'],
      [{ n: Object.assign(new Number(7), { [Symbol.toPrimitive](hint: string) { return hint; } }) }, '{"n":null}'],
      [{ s: Object.assign(new String("x"), { [Symbol.toPrimitive]() { return "y"; } }) }, '{"s":"y"}'],
      [{ n: Object.assign(new Number(7), { [Symbol.toPrimitive]() { return {}; } }) }, null],
      // String wrappers prefer toString (string hint) over valueOf.
      [{ s: Object.assign(new String("x"), { toString() { return "y"; } }) }, '{"s":"y"}'],
      // Number wrappers run ToPrimitive with the number hint: shadowing valueOf
      // wins and the result is coerced to a number.
      [{ n: Object.assign(new Number(7), { valueOf() { return 42; } }) }, '{"n":42}'],
      [{ n: Object.assign(new Number(7), { valueOf() { return "s"; } }) }, '{"n":null}'],
      [{ n: Object.assign(new Number(7), { valueOf() { return null; } }) }, '{"n":0}'],
      [{ n: Object.assign(new Number(7), { valueOf() { return true; } }) }, '{"n":1}'],
      // String and Boolean wrappers read internal slots, ignoring shadowing valueOf.
      [{ s: Object.assign(new String("x"), { valueOf() { return "y"; } }) }, '{"s":"x"}'],
      [{ b: Object.assign(new Boolean(false), { valueOf() { return true; } }) }, '{"b":false}'],
      // Slotless inheritors of wrapper prototypes are plain objects to native JSON.
      [{ n: Object.create(Number.prototype) }, '{"n":{}}'],
      [{ b: Object.create(BigInt.prototype) }, '{"b":{}}'],
      // A plain object with a numeric valueOf is object-walked, never ToPrimitive'd.
      [{ valueOf() { return 9; }, x: 1 }, '{"x":1}'],
    ];
    for (const [payload, expected] of cases) {
      const rec = new RecordingStdin();
      if (expected === null) {
        await expect(streamJsonTo(asChild(rec), payload)).rejects.toThrow(/Cannot convert object to primitive value/);
        continue;
      }
      await streamJsonTo(asChild(rec), payload);
      expect(rec.text()).toBe(expected);
    }
  });

  it("matches JSON.stringify on a seeded random corpus", async () => {
    let seed = 0xc0ffee;
    const rand = (): number => {
      seed = (seed * 1664525 + 1013904223) >>> 0;
      return seed / 0xffffffff;
    };
    const pick = <T,>(arr: T[]): T => arr[Math.floor(rand() * arr.length)];
    const gen = (depth: number): unknown => {
      if (depth <= 0) return pick([null, "leaf", 1]);
      const choices: Array<() => unknown> = [
        () => pick(["", "αβ", "日本語", 'quote"back\\slash', "\n\t", "x".repeat(64)]),
        () => pick([0, -0, 1.5, -3, NaN, Infinity, -Infinity, 1e21]),
        () => pick([null, true, false]),
        () => undefined,
        () => pick([() => 0, Symbol("s")]),
        () => ({ toJSON() { return undefined; } }),
        () => ({ toJSON() { return 42; } }),
        () => new Date(1_700_000_000_000),
        () => new Number(7),
        () => new String("boxed"),
        () => new Boolean(true),
        () => {
          const a = new Array(3);
          a[1] = pick([1, "x", undefined] as unknown[]);
          return a;
        },
        () => {
          const o: Record<string, unknown> = {};
          for (let i = 0; i < 3; i++) o["k" + i] = gen(depth - 1);
          return o;
        },
        () => Array.from({ length: 3 }, () => gen(depth - 1)),
      ];
      return pick(choices)();
    };
    for (let i = 0; i < 500; i++) {
      const value = gen(4);
      let expected: string | undefined;
      let expectedThrows = false;
      try {
        expected = JSON.stringify(value);
      } catch {
        expectedThrows = true;
      }
      const rec = new RecordingStdin();
      let actualThrows = false;
      try {
        await streamJsonTo(asChild(rec), value);
      } catch {
        actualThrows = true;
      }
      expect(`#${i} throws=${actualThrows}`).toBe(`#${i} throws=${expectedThrows}`);
      expect(`#${i} out=${actualThrows ? "-" : rec.text()}`).toBe(`#${i} out=${expectedThrows ? "-" : (expected ?? "")}`);
    }
  }, 30_000);

  it("invokes toJSON exactly once per property like native JSON", async () => {
    let calls = 0;
    const rec = new RecordingStdin();
    await streamJsonTo(asChild(rec), { inner: { toJSON() { calls++; return { n: calls }; } } });
    expect(rec.text()).toBe('{"inner":{"n":1}}');
    expect(calls).toBe(1);
  });

  it("reads property values lazily, observing mutations from earlier toJSON hooks", async () => {
    const parent: Record<string, unknown> = {
      a: { toJSON() { parent.b = 2; return 0; } },
      b: 1,
    };
    const rec = new RecordingStdin();
    await streamJsonTo(asChild(rec), parent);
    expect(rec.text()).toBe('{"a":0,"b":2}');
  });

  it("captures array length before the walk like native JSON", async () => {
    const array: unknown[] = [{ toJSON() { array.length = 1; return 0; } }, 1];
    const rec = new RecordingStdin();
    await streamJsonTo(asChild(rec), array);
    expect(rec.text()).toBe("[0,null]");
  });

  it("omits properties deleted by an earlier getter during the walk", async () => {
    const parent: Record<string, unknown> = {
      get a() {
        delete parent.b;
        return 5;
      },
      b: 1,
    };
    const rec = new RecordingStdin();
    await streamJsonTo(asChild(rec), parent);
    expect(rec.text()).toBe('{"a":5}');
  });

  it("rejects boxed BigInt values like native JSON", async () => {
    const rec = new RecordingStdin();
    await expect(streamJsonTo(asChild(rec), { x: Object(1n) })).rejects.toThrow(/BigInt/);
  });

  it("omits function and symbol property values and nulls them in arrays", async () => {
    const cases: Array<[unknown, string]> = [
      [{ x: () => 0, y: Symbol("s"), kept: 1 }, '{"kept":1}'],
      [[() => 0, undefined, Symbol("s")], "[null,null,null]"],
    ];
    for (const [payload, expected] of cases) {
      const rec = new RecordingStdin();
      await streamJsonTo(asChild(rec), payload);
      expect(rec.text()).toBe(expected);
    }
  });

  it("unboxes Number, String, and Boolean wrapper objects like native JSON", async () => {
    const rec = new RecordingStdin();
    await streamJsonTo(asChild(rec), { x: new Number(1), y: new String("s"), z: new Boolean(false) });
    expect(rec.text()).toBe('{"x":1,"y":"s","z":false}');
  });

  it("rejects bigints loudly like native JSON", async () => {
    const rec = new RecordingStdin();
    await expect(streamJsonTo(asChild(rec), { bad: 10n })).rejects.toThrow(TypeError);
    await expect(streamJsonTo(asChild(rec), { bad: 10n })).rejects.toThrow(/BigInt/);
  });

  it("writes nothing for a root value JSON.stringify would map to undefined", async () => {
    for (const value of [undefined, () => 0, Symbol("root")]) {
      const rec = new RecordingStdin();
      await streamJsonTo(asChild(rec), value);
      expect(rec.totalLength).toBe(0);
      expect(rec.ended).toBe(true);
    }
  });

  it("keeps emitting when the payload exceeds V8 max string length", async () => {
    // One JSON.stringify of this payload throws RangeError; the streamed
    // writer must assemble it from per-leaf strings instead.
    const entryText = "y".repeat(1024 * 1024);
    const payload = {
      entries: Array.from({ length: 560 }, (_, i) => ({ id: `e${i}`, text: entryText })),
    };
    expect(() => JSON.stringify(payload)).toThrow(RangeError);
    const rec = new RecordingStdin();
    await streamJsonTo(asChild(rec), payload);
    // Assert without materializing the giant output — that is the bug class.
    expect(rec.totalLength).toBeGreaterThan(V8_MAX_STRING_LENGTH);
    expect(rec.head.startsWith("{\"entries\":[")).toBe(true);
    expect(rec.tail.endsWith("]}")).toBe(true);
    expect(rec.ended).toBe(true);
  }, 30_000);
});

describe("runUltraCompress stdin streaming", () => {
  const dir = mkdtempSync(join(tmpdir(), "uc-bridge-"));
  const consumerScript = join(dir, "consumer.cjs");

  afterAll(() => {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {}
  });

  const writeConsumer = (): void => {
    // Prints the number of stdin bytes received; proves the payload crossed the pipe intact.
    writeFileSync(
      consumerScript,
      "let n=0;process.stdin.on('data',d=>n+=d.length);process.stdin.on('end',()=>{process.stdout.write(JSON.stringify({bytes:n}))});",
    );
  };

  it("delivers the payload byte-identically to the child process", async () => {
    writeConsumer();
    const payload = {
      entries: [
        { type: "message", role: "user", text: "hello α β" },
        { type: "message", role: "assistant", text: "hi".repeat(10_000) },
      ],
      policy: "snap",
    };
    const expected = Buffer.byteLength(JSON.stringify(payload));
    const res = await runUltraCompress<{ bytes: number }>(process.execPath, [consumerScript], payload);
    expect(res.ok).toBe(true);
    expect(res.data?.bytes).toBe(expected);
  });

  it("streams payloads beyond V8 max string length without RangeError", async () => {
    writeConsumer();
    const entryText = "y".repeat(1024 * 1024);
    const entries = Array.from({ length: 560 }, (_, i) => ({ id: `e${i}`, text: entryText }));
    const payload = { entries, policy: "snap" };
    // The old path: one JSON.stringify throws before anything reaches the pipe.
    expect(() => JSON.stringify(payload)).toThrow(RangeError);
    // Expected byte length computed entry by entry, never one giant string.
    let expected = Buffer.byteLength('{"entries":[');
    for (let i = 0; i < entries.length; i++) {
      if (i > 0) expected += 1;
      expected += Buffer.byteLength(JSON.stringify(entries[i]));
    }
    expected += Buffer.byteLength('],"policy":"snap"}');
    const res = await runUltraCompress<{ bytes: number }>(process.execPath, [consumerScript], payload, 120_000);
    expect(res.ok).toBe(true);
    expect(res.data?.bytes).toBe(expected);
    expect(res.data?.bytes).toBeGreaterThan(V8_MAX_STRING_LENGTH);
  }, 150_000);

  it("survives backpressure from a stalled consumer, then folds EPIPE into the result", async () => {
    // Consumes nothing, waits, then exits 0 while the writer still has queued payload.
    writeFileSync(consumerScript, "setTimeout(()=>process.exit(0), 300);");
    const entryText = "y".repeat(1024 * 1024);
    const payload = { entries: Array.from({ length: 300 }, (_, i) => ({ id: `e${i}`, text: entryText })) };
    const res = await runUltraCompress(process.execPath, [consumerScript], payload, 30_000);
    expect(res.ok).toBe(false);
    expect(res.error).toContain("stdin write failed");
  }, 60_000);

  it("decodes stdout multi-byte characters split across chunk boundaries", async () => {
    // Byte 10 lands inside the 3-byte 日 — naive per-chunk toString corrupts it.
    writeFileSync(
      join(dir, "split.cjs"),
      "const b=Buffer.from('{\\\"out\\\":\\\"日本語\\\"}');process.stdout.write(b.subarray(0,10));setTimeout(()=>{process.stdout.write(b.subarray(10));process.stdout.end();},50);",
    );
    const res = await runUltraCompress<{ out: string }>(process.execPath, [join(dir, "split.cjs")], { p: 1 }, 15_000);
    expect(res.ok).toBe(true);
    expect(res.data?.out).toBe("日本語");
  });

  it("fails with the stdin error prefix instead of crashing when serialization is impossible", async () => {
    writeFileSync(consumerScript, "process.stdin.resume();");
    const res = await runUltraCompress(process.execPath, [consumerScript], { bad: 10n }, 10_000);
    expect(res.ok).toBe(false);
    expect(res.error).toContain("stdin write failed");
  });

  it("preserves the WriterCancelled race: timeout wins while a huge payload is mid-stream", async () => {
    // Consumes slowly enough that the writer is still draining at timeout.
    writeFileSync(consumerScript, "let n=0;process.stdin.on('data',d=>{n+=d.length;setTimeout(()=>{},1);});process.stdin.on('end',()=>{});setTimeout(()=>process.exit(0),5000);");
    const entryText = "y".repeat(1024 * 1024);
    const payload = { entries: Array.from({ length: 400 }, (_, i) => ({ id: `e${i}`, text: entryText })) };
    const res = await runUltraCompress(process.execPath, [consumerScript], payload, 1_500);
    expect(res.ok).toBe(false);
    expect(res.error).toContain("timed out");
  }, 30_000);
});
