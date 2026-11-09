import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// A runtime name in the `key` field of a structural key handed to unseen code
// is not tracked (Tim's decision, 2026-11-09), so the held and late lookups
// keep nothing: their method definitions go, and the bundle is not run, since
// runtime equivalence is not required for code the decision excludes. The
// literal-named structural key is the control: its read method stays and its
// unread one goes.
//
// The program's own call sites stay in the bundle, so the checks look for the
// method definitions (`name:` in the service object), not the bare name.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const defined = (name) => new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code);
const failures = [];
for (const name of ['heldUsed', 'heldUnused', 'lateUsed', 'lateUnused', 'literalUnused']) {
  if (defined(name)) failures.push(`the definition of ${name} should be removed`);
}
if (!defined('literalUsed')) failures.push('the definition of literalUsed should be kept');
assert.deepStrictEqual(failures, []);
