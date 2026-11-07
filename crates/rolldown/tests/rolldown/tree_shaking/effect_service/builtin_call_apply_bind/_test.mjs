import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// Each method read through an Effect function's .call, .apply or .bind stays.
// Every name is checked before the program runs, so one case failing does not
// hide the others.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
const failures = [];
for (const name of ['viaPipeCall', 'viaMapApply', 'viaMapBind']) {
  if (!new RegExp(`(?<![.\\w$])${name}\\s*:`).test(code)) failures.push(`${name} should be kept`);
}
for (const name of ['builtinUnused', 'builtinHelper']) {
  if (new RegExp(`\\b${name}\\b`).test(code)) failures.push(`${name} should be removed`);
}
assert.deepStrictEqual(failures, []);

// The program asserts its own output when imported.
await import('./dist/main.js');
