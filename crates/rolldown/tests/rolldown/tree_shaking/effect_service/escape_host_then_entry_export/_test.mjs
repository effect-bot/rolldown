import assert from 'node:assert/strict';

process.zzHost = (s) => s;
const mod = await import('./dist/main.js');
assert.strictEqual(mod.svc.unused(), 'unused');
