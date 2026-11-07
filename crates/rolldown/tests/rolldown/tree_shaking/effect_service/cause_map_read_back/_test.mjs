import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const kept = [
  ['mapped', /(?<![.\w$])mapped\s*[:(]/],
  ['discovered', /(?<![.\w$])discovered\s*[:(]/],
  ['viaNestedMap', /(?<![.\w$])viaNestedMap\s*[:(]/],
  ['viaRun', /(?<![.\w$])viaRun\s*[:(]/],
  ['viaAnnotation', /(?<![.\w$])viaAnnotation\s*[:(]/],
];
const removed = [
  ['inputUnused', /\binputUnused\b/],
  ['madeUnused', /\bmadeUnused\b/],
  ['nestedUnused', /\bnestedUnused\b/],
  ['boundUnused', /\bboundUnused\b/],
  ['annotatedUnused', /\bannotatedUnused\b/],
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
