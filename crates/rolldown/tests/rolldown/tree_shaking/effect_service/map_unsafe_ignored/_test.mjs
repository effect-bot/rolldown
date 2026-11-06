import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Reads through `mapUnsafe` keep nothing, so the methods read only there go
// and the bundle is not run: it would call a removed method. The program runs
// unbundled. The program's call sites stay in the bundle, so the check looks
// for the method definitions (`name:` in the service object).
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const definition = (name) => new RegExp(`(?<![.\\w$])${name}\\s*:`);
for (const name of ['readOne']) {
  assert.match(code, definition(name), `the definition of ${name} should be kept`);
}
for (const name of ['viewRead', 'viewComputed', 'viewUnused']) {
  assert.doesNotMatch(code, definition(name), `the definition of ${name} should be removed`);
}
for (const name of ['viewUnusedHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
