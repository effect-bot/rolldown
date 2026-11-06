import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// The alias key reads the service provided under the overriding string, so the
// method it calls stays. Nothing reads the other method: it is removed.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of [
  'overridesKeyUnused',
  'overridesKeyUnusedHelper',
]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of [
  'overridesKeyUnread',
  'overridesKeyUnreadHelper',
]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
