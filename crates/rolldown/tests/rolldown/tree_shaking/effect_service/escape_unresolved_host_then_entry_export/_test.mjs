import assert from 'node:assert/strict';

globalThis.zzHost = (s) => s;
const mod = await import('./dist/main.js');
assert.strictEqual(mod.svc.flUnused(), 'unused'); // TypeError at 68dff5ac: mod.svc.flUnused is not a function
