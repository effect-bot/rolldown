import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Structural equality and hashing walks keep nothing; the service's own [Equal.symbol]/[Hash.symbol] stay (checked by the program's results).
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['readOne', 'readTwo']) {
  assert.match(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be kept`);
}
for (const name of ['equalUnused', 'ownedUnused']) {
  assert.doesNotMatch(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be removed`);
}
