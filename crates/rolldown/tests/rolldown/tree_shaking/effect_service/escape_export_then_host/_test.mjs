import assert from 'node:assert/strict';

process.zzHost = (f) => {
  globalThis.escapeExportThenHost = f('test/EscapeExportThenHost');
};
await import('./dist/main.js');
assert.strictEqual(globalThis.escapeExportThenHost, 'used');
