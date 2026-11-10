import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['budgetRead', 'budgetUnread']) {
  assert.match(code, new RegExp(`(?<![.\\w$])${name}\\s*:`), `${name} should be kept`);
}

// The program asserts its own output when imported.
await import('./dist/main.js');
