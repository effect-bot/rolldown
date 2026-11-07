import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Both read-back paths reach the service; nothing reads causeUnused.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['readCaught', 'readFound']) {
  assert.match(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be kept`);
}
for (const name of ['causeUnused', 'causeUnusedHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
