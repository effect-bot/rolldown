import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each loader is reported on its
// own rather than only as the bundle's first failure.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['holeGet', 'holeImport', 'holeCommonJs', 'holeRequire', 'holeDynamic']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code)) failures.push(`${name} should be kept`);
}
if (/\bholeUnused\b/.test(code)) failures.push('holeUnused should be removed');
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
