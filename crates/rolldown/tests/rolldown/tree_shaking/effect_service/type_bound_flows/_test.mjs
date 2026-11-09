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
  // ProtoHandlers' method closes over the Inherited service, so the service reaches class
  // code and its key keeps every method (Tim's class rule).
  ['inheritedUnused', /(?<![.\w$])inheritedUnused\s*[:(]/],
];
// `handlersUnused` is not checked: `Option.match(_, handlers)` hands the service to
// type-modelled code as an options object, whose members all count as read (design.md,
// the options-object rule), so it stays.
const removed = [
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
