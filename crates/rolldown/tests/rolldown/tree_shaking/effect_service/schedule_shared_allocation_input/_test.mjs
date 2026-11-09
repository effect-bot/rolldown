import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Every name is checked before the program runs, so a failure to run does not
// hide which method went.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
if (!/(?<![.\w$])sharedUsed\s*:/.test(code)) failures.push('sharedUsed should be kept');
// sharedUnused is not checked: `Schedule.fromStep`'s step is a callback handed to
// type-modelled code, so it runs with unknown arguments, and the computed read
// through its input is an unknown key that keeps every method (accepted
// imprecision; the pass no longer steps a Schedule).
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
