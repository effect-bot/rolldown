import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
// The protocol method is matched by its value, since its key is computed.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
if (!/(?<![.\w$])respondUsed\s*:/.test(code)) failures.push('respondUsed should be kept');
if (!/status:\s*201\b/.test(code)) {
  failures.push('[HttpServerRespondable.symbol] should be kept');
}
if (/\brespondUnused\b/.test(code)) failures.push('respondUnused should be removed');
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
