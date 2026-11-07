import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The dead code's names may be mangled, but `unreachable` is a global: it
// stays in the output for as long as any fan-in link or chain link does.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.doesNotMatch(code, /\bunreachable\b/);

const { calls, deep } = await import('./dist/main.js');
assert.deepStrictEqual(calls, ['common']);
let result = deep;
for (let i = 0; i < 16; i++) result = result();
assert.deepStrictEqual(calls, ['common', 'shared']);
