import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Every name is checked before the program runs, so a failed run does not hide
// a retention result.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['inSpan']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code)) failures.push(`${name} should be kept`);
}
for (const name of ['spanUnused', 'spanUnusedHelper']) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
