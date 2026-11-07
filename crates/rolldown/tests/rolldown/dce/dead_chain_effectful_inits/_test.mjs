import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The links' names may be mangled, but `unreachable` is a global: it stays in
// the output for as long as any link does, since each one reaches link0.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.doesNotMatch(code, /\bunreachable\b/);

const { ticks } = await import('./dist/main.js');
assert.deepStrictEqual(ticks, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
