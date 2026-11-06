import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Each piped read reaches only its own receiver's service.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['foo', 'bar']) {
  assert.match(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be kept`);
}
for (const name of ['aBarHelper', 'bFooHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
