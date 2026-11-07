import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as FileSystem from 'effect/FileSystem';
import * as HttpBody from 'effect/http/HttpBody';

// A class method is the only caller of `HttpBody.file`, which calls the
// FileSystem service's `stat` to size the body. HttpBody's type-modelled code
// still counts as a reader of the FileSystem service: the key keeps every
// method, `stat` included.
const impl = {
  stat: () => Effect.succeed({ size: 5n }),
  remove: () => Effect.void,
};

class Uploader {
  body(path) {
    return HttpBody.file(path);
  }
}

const program = Effect.gen(function* () {
  const body = yield* new Uploader().body('/greeting.txt');
  return body.contentLength;
});

const length = await Effect.runPromise(
  Effect.provideService(program, FileSystem.FileSystem, FileSystem.make(impl)),
);
assert.strictEqual(length, 5);
