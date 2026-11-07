import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
// Services from this constructor are not pruned: every method stays.
const kept = [
  ['get', /(?<![.\w$])get\s*:\s*\(?key\)?\s*=>/],
  ['contextEffect', /(?<![.\w$])contextEffect\s*:\s*\(?key\)?\s*=>/],
  ['contextEffectOption', /(?<![.\w$])contextEffectOption\s*:\s*\(?key\)?\s*=>/],
  ['invalidate', /(?<![.\w$])invalidate\s*:\s*\(?key\)?\s*=>/],
];
const failures = [];
for (const [name, pattern] of kept) {
  if (!pattern.test(code)) failures.push(`${name} should be kept`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
