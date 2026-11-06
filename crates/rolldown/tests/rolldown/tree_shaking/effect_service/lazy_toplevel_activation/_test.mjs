import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported: a reader whose statement
// the analysis skipped would have lost its method, and the bundle would throw.
await import('./dist/main.js');

const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of [
  'transitiveUsed',
  'namespaceUsed',
  'barrelUsed',
  'wholesaleUsed',
  'initializerUsed',
]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of ['lazyUnused', 'lazyUnusedHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
