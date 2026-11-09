import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The name is checked before the program runs, so a failure to run does not
// hide which method went. The unread method may stay: the Schedule's output is
// an unknown scalar (assumption 4), so the computed read is an unknown key.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
if (!/(?<![.\w$])metadataOutputUsed\s*:/.test(code))
  failures.push('metadataOutputUsed should be kept');
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
