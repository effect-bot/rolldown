import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The program asserts its own output when imported.
await import('./dist/main.js');

// Over the budget, the analysis keeps every method.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['budgetUsed', 'budgetUnused']) {
  assert.match(code, new RegExp(`\\b${name}\\s*:`), `the definition of ${name} should be kept`);
}
