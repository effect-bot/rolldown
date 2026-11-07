import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
// The protocol methods are matched by their values, since effect has its own
// toJSON members and the symbol keys are computed.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const kept = [
  ['jsonUsed', /(?<![.\w$])jsonUsed\s*:/],
  ['redactedUsed', /(?<![.\w$])redactedUsed\s*:/],
  ['toJSON', /toJSON\s*:\s*\(\)\s*=>\s*["']json protocol["']/],
  ['[Redactable.symbolRedactable]', /=>\s*["']redacted protocol["']/],
];
const removed = [
  ['jsonUnused', /\bjsonUnused\b/],
  ['redactedUnused', /\bredactedUnused\b/],
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
