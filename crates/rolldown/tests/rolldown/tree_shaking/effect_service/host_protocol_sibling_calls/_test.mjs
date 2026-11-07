import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Every case is checked before the program runs, so one failure does not hide
// the others: each protocol method's helper must stay, each unread method go.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['stringHelper', 'jsonHelper', 'logHelper', 'inspectHelper']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code)) failures.push(`${name} should be kept`);
}
for (const name of ['stringUnused', 'jsonUnused', 'logUnused', 'inspectUnused']) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
