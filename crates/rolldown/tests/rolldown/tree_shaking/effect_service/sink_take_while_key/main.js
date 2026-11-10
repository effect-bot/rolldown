import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Sink from 'effect/Sink';
import * as Stream from 'effect/Stream';
// A service-free string collected by `Sink.takeWhile` comes back in the Sink's
// result and names a method of a service the program holds. A Sink's result is
// untracked (assumption 6) but comes back as an unknown scalar, which may name
// any method, so the named method stays and the bundle runs. (The unread method
// may stay too, an accepted imprecision; it is not checked.)
const Store = Context.Service('test/SinkTakeWhileKey');
const program = Effect.gen(function* () {
  const store = yield* Store;
  const keys = yield* Stream.make('takeKeyUsed').pipe(Stream.run(Sink.takeWhile(() => true)));
  return keys.map((key) => store[key]());
});
assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.succeed(Store)({ takeKeyUsed: () => 'used', takeKeyUnused: () => 'unused' }),
    ),
  ),
  ['used'],
);
