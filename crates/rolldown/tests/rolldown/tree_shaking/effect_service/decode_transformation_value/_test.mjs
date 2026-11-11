import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
// decodedUnused is kept since the escape batch. Unfinished: the record escapes
// to a host builtin and then into effect's code, and an escape into effect's
// code keeps every method. Its removal held only while the host escape arrived
// first and the later escape was skipped.
const kept = [
  ['decodedUsed', /(?<![.\w$])decodedUsed\s*:/],
  ['decodedUnused', /(?<![.\w$])decodedUnused\s*:/],
];
const removed = [];
const failures = [];
for (const [name, pattern] of kept) {
  if (!pattern.test(code)) failures.push(`${name} should be kept`);
}
for (const [name, pattern] of removed) {
  if (pattern.test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
