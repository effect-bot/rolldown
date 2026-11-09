import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The external module only appears now, next to the bundle. It hands back the
// value it is given, or the one it holds.
fs.writeFileSync(
  path.join(import.meta.dirname, 'dist/late.js'),
  [
    'export const identity = (value) => value;',
    'let held;',
    'export const hold = (value) => { held = value; };',
    'export const take = () => held;',
    '',
  ].join('\n'),
);

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
const cases = ['held', 'late', 'literal'];
for (const name of cases.map((c) => `${c}Used`)) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*[:(]`).test(code))
    failures.push(`${name} should be kept`);
}
for (const name of cases.map((c) => `${c}Unused`)) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
process.env.EXTERNAL_STRUCTURAL_HELD_NAME = 'test/ExternalStructuralHeld';
process.env.EXTERNAL_STRUCTURAL_LATE_NAME = 'test/ExternalStructuralLate';
await import('./dist/main.js');
