import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The service is looked up only by a runtime key name (`process.env`) that an
// external module hands back. A runtime name string returned by unseen code is
// not read as a key's name (Tim's decision, 2026-11-09), so that lookup keeps
// nothing: both methods may go and the bundle is not run, since runtime
// equivalence is not required for code the decision excludes.
//
// The program's own call site stays in the bundle, so the check looks for the
// method definitions (`name:` in the service object), not the bare name.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['used', 'unused']) {
  assert.doesNotMatch(
    code,
    new RegExp(`(?<![.\\w$])${name}\\s*:`),
    `the definition of ${name} should be removed`,
  );
}
