import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported: `HttpBody.file` calls `stat`.
await import('./dist/main.js');

const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.match(code, /\bstat\b/, 'stat should be kept');
