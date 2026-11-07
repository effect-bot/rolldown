import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The services reach the decoded values only through decodeUnknownSync's
// `unknown` input, which holds no service in the result, so the methods called
// on the decoded values may go and the bundle is not run: runtime equivalence
// is not required for code the assumption excludes. The program still runs
// unbundled.
//
// The program's own call sites stay in the bundle, so the check looks for the
// method definitions (`name:` in the service object), not the bare name.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['flush', 'greet']) {
  if (new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code)) {
    failures.push(`the definition of ${name} should be removed`);
  }
}
assert.deepStrictEqual(failures, []);
