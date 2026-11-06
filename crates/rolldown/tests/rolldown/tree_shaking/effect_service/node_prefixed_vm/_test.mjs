import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// The service handed to compiled code bails its key: every method stays.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['vmUnused', 'vmUnusedHelper']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
