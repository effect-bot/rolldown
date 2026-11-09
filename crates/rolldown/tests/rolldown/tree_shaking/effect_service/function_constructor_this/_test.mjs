import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// The methods read through the constructed objects are kept; the unread Box method is removed.
// `someUnused` is not checked: `Option.some`'s argument is a record handed to type-modelled
// code at a type parameter's position, whose members all count as read (accepted imprecision).
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['boxUsed', 'someUsed']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of ['unusedBox']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
