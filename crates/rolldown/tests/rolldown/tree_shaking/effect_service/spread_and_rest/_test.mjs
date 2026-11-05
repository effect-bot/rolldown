import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Spread and rest are precise; enumeration of the rest bails that key only.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of [
  'readOne',
  'spreadExtra',
  'enumeratedRead',
  'enumeratedUnused',
  'enumeratedUnusedHelper',
]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of ['spreadImplUnused', 'spreadImplUnusedHelper', 'spreadExtraUnused']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
