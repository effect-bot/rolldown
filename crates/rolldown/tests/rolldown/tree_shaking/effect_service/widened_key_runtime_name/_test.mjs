import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
const cases = ['early', 'late', 'objectEarly', 'objectLate'];
for (const name of cases.map((c) => `${c}Used`)) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*[:(]`).test(code))
    failures.push(`${name} should be kept`);
}
for (const name of cases.map((c) => `${c}Unused`)) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
process.env.WIDENED_EARLY_NAME = 'test/WidenedRuntimeEarly';
process.env.WIDENED_LATE_NAME = 'test/WidenedRuntimeLate';
process.env.WIDENED_OBJECT_EARLY_NAME = 'test/WidenedRuntimeObjectEarly';
process.env.WIDENED_OBJECT_LATE_NAME = 'test/WidenedRuntimeObjectLate';
await import('./dist/main.js');
