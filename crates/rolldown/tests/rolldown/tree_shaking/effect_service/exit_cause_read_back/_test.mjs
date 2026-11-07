import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so a removed sibling is reported
// by name rather than only as the bundle's TypeError.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['readExit', 'exitHelper']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*[:(]`).test(code))
    failures.push(`${name} should be kept`);
}
for (const name of ['exitUnused', 'exitUnusedHelper']) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
