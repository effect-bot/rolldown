import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The service is read through `.value` on an Effect, which does not type check
// against Effect's public types. That read keeps nothing, so both methods may go
// and the bundle is not run: runtime equivalence is not required for code the
// assumption excludes. The program still runs unbundled.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['probeUsed', 'probeUnused']) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`);
}
