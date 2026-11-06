import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The MemoMap use keeps nothing, so `memoUnused` goes and the bundle is not
// run: what the program reads through the MemoMap is not checked. The program
// runs unbundled. Its call site stays in the bundle, so the check looks for
// the method definitions (`name:` in the service object).
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const definition = (name) => new RegExp(`(?<![.\\w$])${name}\\s*:`);
assert.match(code, definition('readOne'), 'the definition of readOne should be kept');
assert.doesNotMatch(
  code,
  definition('memoUnused'),
  'the definition of memoUnused should be removed',
);
assert.doesNotMatch(code, /\bmemoUnusedHelper\b/, 'memoUnusedHelper should be removed');
