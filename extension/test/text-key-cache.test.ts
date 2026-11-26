import { expect, it, vi } from "vitest";
import { TextKeyCache } from "../src/text-key-cache.ts";

it("memoizes exact strings, invalidates scopes and clears session state", () => {
  const cache = new TextKeyCache();
  const compute = vi.fn(() => "key");
  cache.setScope("a");
  cache.get("original", compute);
  cache.get(structuredClone("original"), compute);
  expect(compute).toHaveBeenCalledTimes(1);
  cache.get("mutated!", compute);
  cache.setScope("b");
  cache.get("original", compute);
  cache.clear();
  cache.get("original", compute);
  expect(compute).toHaveBeenCalledTimes(4);
});

it("bounds retained characters and entries, refreshes LRU, bypasses oversize text", () => {
  const cache = new TextKeyCache(6, 2);
  const compute = vi.fn(() => "key");
  for (const text of ["aa", "bb", "aa", "cc", "aa"]) cache.get(text, compute);
  expect(compute).toHaveBeenCalledTimes(3);
  cache.get("bb", compute);
  cache.get("123456", compute);
  cache.get("bb", compute);
  cache.get("oversize", compute);
  cache.get("oversize", compute);
  expect(compute).toHaveBeenCalledTimes(8);
});
