import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as FileSystem from 'effect/FileSystem';
import * as HttpBody from 'effect/http/HttpBody';

// `JSON.stringify` calls a `toJSON` that is an effect export, `HttpBody.file`.
// Its module imports `effect/FileSystem` and reads the service's `stat`, so
// reaching it as a protocol method still makes it a reader of that key: the
// FileSystem service keeps every method, `stat` included.
const impl = {
  stat: () => Effect.succeed({ size: 5n }),
  remove: () => Effect.void,
};

const printed = JSON.stringify({ toJSON: HttpBody.file });

const program = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  return typeof fs;
});

const kind = await Effect.runPromise(
  Effect.provideService(program, FileSystem.FileSystem, FileSystem.make(impl)),
);
assert.strictEqual(kind, 'object');
assert.strictEqual(typeof printed, 'string');
