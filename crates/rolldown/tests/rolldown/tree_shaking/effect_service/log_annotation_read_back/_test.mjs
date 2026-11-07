import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The service is read back from log annotations, which Effect's public types
// give as `unknown`. Such a value holds no service even after narrowing, so
// both read-back methods may go with the unread one, and the bundle is not run:
// runtime equivalence is not required for code the assumption excludes. The
// program still runs unbundled.
//
// The program's own call sites stay in the bundle, so the check looks for the
// method definitions (`name:` in the service object), not the bare name.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['readReference', 'readLogger', 'annotationUnused']) {
  if (new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code)) {
    failures.push(`the definition of ${name} should be removed`);
  }
}
if (/\bannotationUnusedHelper\b/.test(code))
  failures.push('annotationUnusedHelper should be removed');
assert.deepStrictEqual(failures, []);
