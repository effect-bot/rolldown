import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program puts services into Effect's error channel, which the pass assumes
// never happens (Tim's decision): reads through it keep nothing, so the bundle
// is not run. The program's call sites stay in the bundle, so the checks look
// for the method definitions (`name:` in a service object), not the bare names.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const defined = (name) => new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code);
const failures = [];
if (!defined('viaDirect')) failures.push('viaDirect should be kept');
for (const name of [
  'viaCatch',
  'catchUnused',
  'viaDefect',
  'defectUnused',
  'viaTry',
  'tryUnused',
  'viaJoin',
  'joinUnused',
  'viaExitCause',
  'exitUnused',
  'viaMapInput',
  'mapUnused',
  'viaFailureContext',
  'contextUnused',
  'directUnused',
]) {
  if (defined(name)) failures.push(`the definition of ${name} should be removed`);
}
assert.deepStrictEqual(failures, []);
