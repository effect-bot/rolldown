import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
assert.deepStrictEqual(code.match(/\blink\d+\b/g), null);

const { live } = await import('./dist/main.js');
assert.strictEqual(live, 'live');
