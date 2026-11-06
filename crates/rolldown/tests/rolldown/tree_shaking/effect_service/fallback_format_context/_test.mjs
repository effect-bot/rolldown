import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Formatter.format walks the Store service's keys, which keeps nothing.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
// The service literal is written with shorthand properties.
for (const name of ['readOne']) {
  assert.match(code, new RegExp(`(?<![.\\w$])${name}\\s*[:,}]`), `${name} should be kept`);
}
for (const name of ['formatUnused', 'formatUnusedHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
