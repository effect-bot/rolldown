import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as FileSystem from 'effect/FileSystem';
import * as HttpBody from 'effect/http/HttpBody';

// `effect/http/HttpBody` imports `effect/FileSystem`, and `HttpBody.file` calls
// the FileSystem service's `stat` to size the body. Nothing else reads `stat`.
// The program reaches `HttpBody.file` only through a method of the export
// (`.call`), so HttpBody's type-modelled code still counts as a reader of the
// FileSystem service: the key keeps every method, `stat` included.
const impl = {
  stat: () => Effect.succeed({ size: 5n }),
  remove: () => Effect.void,
};

const program = Effect.gen(function* () {
  const body = yield* HttpBody.file.call(undefined, '/greeting.txt');
  return body.contentLength;
});

const length = await Effect.runPromise(
  Effect.provideService(program, FileSystem.FileSystem, FileSystem.make(impl)),
);
assert.strictEqual(length, 5);
