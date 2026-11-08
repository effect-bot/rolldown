import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The external module only appears now, next to the bundle. It returns the key
// it is handed.
fs.writeFileSync(path.join(import.meta.dirname, 'dist/late.js'), 'export const identity = (key) => key;\n');

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['identityUsed', 'identityOtherUsed']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*[:(]`).test(code)) failures.push(`${name} should be kept`);
}
for (const name of ['identityUnused', 'identityOtherUnused']) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
