import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Putting a service in a Cause or a log annotation keeps nothing.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['readOne']) {
  assert.match(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be kept`);
}
for (const name of ['causeUnused']) {
  assert.doesNotMatch(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be removed`);
}
