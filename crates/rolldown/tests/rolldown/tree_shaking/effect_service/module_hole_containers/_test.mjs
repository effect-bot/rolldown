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
  // Kept since the escape batch. Unfinished: the record escapes to a host
  // builtin and then into effect's code, and an escape into effect's code keeps
  // every method. Its removal held only while the host escape arrived first and
  // the later escape was skipped.
  'omittedUnused',
]) {
  if (!kept(name)) failures.push(`${name} should be kept`);
}
for (const name of ['passedUnused', 'layerUnused', 'builtUnused', 'mappedUnused', 'madeUnused']) {
  if (kept(name)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
