import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The external module only appears now, next to the bundle. It looks at what it
// is given without reading services through it, and reads a method of the
// service handed to it by a name built at run time.
fs.writeFileSync(
  path.join(import.meta.dirname, 'dist/late.js'),
  [
    'export const describe = (...values) => values.filter((v) => v != null).length;',
    'export const readDirect = (direct) => direct[["opaqueDirect", "Unused"].join("")]();',
  ].join('\n') + '\n',
);

// The program asserts its own output when imported.
await import('./dist/main.js');

// Store reaches the external only through runtime objects and loses its unread
// method; Direct is handed over itself and keeps every method.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['readStore', 'readDirect', 'opaqueDirectUnused', 'opaqueDirectUnusedHelper']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of ['opaqueStoreUnused', 'opaqueStoreUnusedHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
