import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The service is read only as a stream element `Sink.takeWhile` collects into
// the Sink's result. Tim's decision (2026-11-10): such a service is not
// tracked, so that read keeps nothing. Both methods may go and the bundle is
// not run, since the built program throws on `takenRead` (Tim accepted that).
// The program still runs unbundled.
//
// The program's own call site stays in the bundle, so the check looks for the
// method definitions (`name:` in the service object), not the bare name.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['takenRead', 'takenUnused']) {
  assert.doesNotMatch(
    code,
    new RegExp(`(?<![.\\w$])${name}\\s*:`),
    `the definition of ${name} should be removed`,
  );
}
