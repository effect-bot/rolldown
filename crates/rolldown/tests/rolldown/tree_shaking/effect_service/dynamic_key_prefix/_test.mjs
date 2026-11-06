import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// The dynamic key may be either prefixed key, so the method read through it
// stays on both. The unprefixed key cannot be read through it, and the
// property name is known, so everything else nothing reads is removed.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['prefixSharedHelperA', 'prefixSharedHelperC']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of [
  'otherPrefixSharedHelper',
  'prefixUnusedA',
  'prefixUnusedHelperA',
  'prefixUnusedC',
  'prefixUnusedHelperC',
  'otherPrefixUnused',
  'otherPrefixUnusedHelper',
]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
