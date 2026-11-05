import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// The second caller of the builder reads the method: it stays.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['builderNamedUnused']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
