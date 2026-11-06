import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Effect's formatting walks keep nothing; the protocol methods it calls stay.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['readOne', 'readTwo', 'toString', 'toJSON']) {
  assert.match(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be kept`);
}
for (const name of ['walkUnused', 'printUnused']) {
  assert.doesNotMatch(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be removed`);
}
