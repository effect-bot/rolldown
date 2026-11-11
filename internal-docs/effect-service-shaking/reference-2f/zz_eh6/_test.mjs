import assert from 'node:assert/strict';
globalThis.zzHost = (f) => {
  globalThis.zzOut = f('test/ZzEh');
};
await import('./dist/main.js');
assert.strictEqual(globalThis.zzOut, 'used');
