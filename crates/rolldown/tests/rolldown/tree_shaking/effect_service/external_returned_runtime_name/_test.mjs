import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The external module only appears now, next to the bundle. It hands back the
// value it was given.
fs.writeFileSync(
  path.join(import.meta.dirname, 'dist/late.js'),
  'export const identity = (value) => value;\n',
);

const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
if (!/(?<![.\w$])used\s*[:(]/.test(code)) failures.push('used should be kept');
if (/\bunused\s*:/.test(code)) failures.push('unused should be removed');
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
process.env.EXTERNAL_RETURNED_NAME = 'test/ExternalReturnedRuntimeName';
await import('./dist/main.js');
