import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// A runtime key string may name either service, so the method read through it
// stays on both. The key is unknown, not the property name: unread methods still go.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of [
  'stringDynamicStoreSharedHelper',
  'stringDynamicOtherSharedHelper',
]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of [
  'stringDynamicStoreUnused',
  'stringDynamicStoreUnusedHelper',
  'stringDynamicOtherUnused',
  'stringDynamicOtherUnusedHelper',
]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
