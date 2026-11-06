import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// The method called on the service that Schema's forked decode returned is kept.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['used']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
