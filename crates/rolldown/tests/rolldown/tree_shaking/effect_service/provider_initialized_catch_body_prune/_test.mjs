import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// The catch binding is initialised in the catch body: the unread property is
// removed.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.doesNotMatch(code, /\bcatchUnused\b/, 'catchUnused should be removed');
