import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// `HttpBody.file`, reached as `toJSON`, may read the FileSystem service's `stat`:
// the implementation keeps its definition.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.match(code, /\bstat:\s*\(\)\s*=>/, 'the implementation should keep stat');
