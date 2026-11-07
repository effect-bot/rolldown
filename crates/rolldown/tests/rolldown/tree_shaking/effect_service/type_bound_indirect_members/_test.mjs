import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const kept = [
  ['viaGetter', /(?<![.\w$])viaGetter\s*[:(]/],
  ['viaAccessor', /(?<![.\w$])viaAccessor\s*[:(]/],
  ['viaAssigned', /(?<![.\w$])viaAssigned\s*[:(]/],
  ['viaStored', /(?<![.\w$])viaStored\s*[:(]/],
  // The class accessor's body closes over the Accessor service, so the service reaches
  // class code and its key keeps every method (Tim's class rule).
  ['accessorUnused', /(?<![.\w$])accessorUnused\s*[:(]/],
];
const removed = [
  ['getterUnused', /\bgetterUnused\b/],
  ['assignedUnused', /\bassignedUnused\b/],
  ['storedUnused', /\bstoredUnused\b/],
];
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
