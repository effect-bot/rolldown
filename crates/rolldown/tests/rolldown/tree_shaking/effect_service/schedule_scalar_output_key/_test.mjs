import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Every name is checked before the program runs, so a failure to run does not
// hide which method went. The unread methods may stay: the program-wide `Done`
// pool also holds a shared primitive, so the computed reads are unknown keys
// (accepted imprecision, design principle 10).
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['repeatUsed', 'retryUsed']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code)) failures.push(`${name} should be kept`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
