import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
// Other handler objects also have `onSome`, so the service's is matched by its value.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const kept = [
  ["the service's onSome", /onSome\s*:\s*\(n\)\s*=>\s*`some/],
  ['viaInherited', /(?<![.\w$])viaInherited\s*[:(]/],
  ['viaLate', /(?<![.\w$])viaLate\s*[:(]/],
  ['viaDeferred', /(?<![.\w$])viaDeferred\s*[:(]/],
];
const removed = [
  ['handlersUnused', /\bhandlersUnused\b/],
  ['inheritedUnused', /\binheritedUnused\b/],
  ['lateUnused', /\blateUnused\b/],
  ['deferredUnused', /\bdeferredUnused\b/],
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
