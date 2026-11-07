import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Names are checked before the program runs, so each case is reported on its
// own rather than only as the bundle's first failure.
// The input struct also has a `count` key, so the service's is matched by its value.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const kept = [["the service's count", /count\s*:\s*\(n\)\s*=>\s*n\s*\+\s*1/]];
const failures = [];
for (const [name, pattern] of kept) {
  if (!pattern.test(code)) failures.push(`${name} should be kept`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
