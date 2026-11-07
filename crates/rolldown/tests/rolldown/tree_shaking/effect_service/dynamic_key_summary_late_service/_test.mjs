import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.match(code, /\bused\b/, 'used should be kept');
