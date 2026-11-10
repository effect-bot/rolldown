import assert from 'node:assert';
import fs from 'node:fs';
import path from 'node:path';

// The service is read only as the result of a Sink that was handed it
// (`Stream.empty.pipe(Stream.run(Sink.succeed(store)))`, data-last). No service
// passes through a Sink's result or leftovers (Tim's decision, assumption 6),
// so that read keeps nothing: both methods may go and the bundle is not run,
// since runtime equivalence is not required for code the assumption excludes
// (the built program throws on `sinkUsed`, which Tim accepted). The program
// still runs unbundled.
//
// The program's own call site stays in the bundle, so the check looks for the
// method definitions (`name:` in the service object), not the bare name.
const code = fs.readFileSync(path.join(import.meta.dirname, 'dist/main.js'), 'utf8');
for (const name of ['sinkUsed', 'sinkUnused']) {
  assert.doesNotMatch(
    code,
    new RegExp(`(?<![.\\w$])${name}\\s*:`),
    `the definition of ${name} should be removed`,
  );
}
