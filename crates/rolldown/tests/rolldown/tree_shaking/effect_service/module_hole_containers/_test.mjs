import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const kept = (name) => new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code);
const failures = [];
for (const name of [
  'passedArg',
  'passedReturn',
  'layerUsed',
  'builtUsed',
  'mappedUsed',
  'madeUsed',
  'omittedUsed',
]) {
  if (!kept(name)) failures.push(`${name} should be kept`);
}
for (const name of [
  'passedUnused',
  'layerUnused',
  'builtUnused',
  'mappedUnused',
  'madeUnused',
  'omittedUnused',
]) {
  if (kept(name)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
