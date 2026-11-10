import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The method the rest-parameter callback calls must stay. The unread method is
// not checked.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.match(code, /(?<![.\w$])restUsed\s*:/, 'restUsed should be kept');

// The program asserts its own output when imported.
await import('./dist/main.js');
