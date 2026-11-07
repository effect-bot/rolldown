import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure. Walked reaches class code
// (`Data.Class`'s constructor), so its key bails: both its methods stay. Owned
// reaches none: the method it calls stays and its unread one is removed; its own [Equal.symbol] and
// [Hash.symbol] stay (checked by the program's results).
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['readOne', 'equalUnused', 'readTwo']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*[:(]`).test(code))
    failures.push(`${name} should be kept`);
}
for (const name of ['ownedUnused']) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
