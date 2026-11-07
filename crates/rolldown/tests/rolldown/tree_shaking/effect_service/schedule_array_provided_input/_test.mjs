import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Every name is checked before the program runs, so a failure to run does not
// hide which method went. A method may be a property or shorthand method.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
if (!/(?<![.\w$])arrayUsed\s*[:(]/.test(code)) failures.push('arrayUsed should be kept');
if (/\barrayUnused\b/.test(code)) failures.push('arrayUnused should be removed');
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
