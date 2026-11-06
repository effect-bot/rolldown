import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The external module only appears now, next to the bundle.
fs.writeFileSync(
  path.join(import.meta.dirname, 'dist/late.js'),
  'export const greeting = (sync) => sync(() => "hello");\n',
);

// The program asserts its own output when imported.
await import('./dist/main.js');

const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['readStore']) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`);
}
for (const name of ['externalRunUnused', 'externalRunUnusedHelper']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
