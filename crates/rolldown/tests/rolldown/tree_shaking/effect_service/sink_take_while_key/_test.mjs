import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The method a `Done` value names must stay. The unread method is not checked:
// with the program-wide `Done` pool it may be kept (accepted imprecision, design
// principle 10).
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.match(code, /(?<![.\w$])takeKeyUsed\s*:/, 'takeKeyUsed should be kept');

// The program asserts its own output when imported.
await import('./dist/main.js');
