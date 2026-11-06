import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The external module only appears now, next to the bundle. It keeps the object
// it is given and later reads a method of the service inside it by a name built
// at run time.
fs.writeFileSync(
  path.join(import.meta.dirname, 'dist/late.js'),
  [
    'let held;',
    'export const register = (value) => { held = value; };',
    'export const readRegistered = () => held.store[["wrappedStore", "Unused"].join("")]();',
  ].join('\n') + '\n',
);

// The program asserts its own output when imported.
await import('./dist/main.js');

// Store reaches the external inside a plain object and keeps every method;
// Other never does and loses its unread one.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['readStore', 'wrappedStoreUnused', 'wrappedStoreUnusedHelper', 'readOther']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of ['wrappedOtherUnused', 'wrappedOtherUnusedHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
