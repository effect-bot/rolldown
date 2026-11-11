import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

fs.mkdirSync(path.join(import.meta.dirname, 'dist/ext'), { recursive: true });
fs.copyFileSync(
  path.join(import.meta.dirname, 'ext/reg.js'),
  path.join(import.meta.dirname, 'dist/ext/reg.js'),
);
process.zzHost = (f) => {
  globalThis.escapeExternalThenHost = f('test/EscapeExternalThenHost');
};
await import('./dist/main.js');
assert.strictEqual(globalThis.escapeExternalThenHost, 'used');
