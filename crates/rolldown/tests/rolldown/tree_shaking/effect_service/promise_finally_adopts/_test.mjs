import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure. The service reaches a
// hand-written thenable that is adopted, so its key bails: every method stays.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const kept = [
  ['finallyUsed', /(?<![.\w$])finallyUsed\s*[:(]/],
  ['finallyUnused', /(?<![.\w$])finallyUnused\s*[:(]/],
];
const failures = [];
for (const [name, pattern] of kept) {
  if (!pattern.test(code)) failures.push(`${name} should be kept`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
