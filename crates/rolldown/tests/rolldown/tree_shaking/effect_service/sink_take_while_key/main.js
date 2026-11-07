import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Sink from 'effect/Sink';
import * as Stream from 'effect/Stream';
// A service-free string collected by `Sink.takeWhile` comes back through the
// Sink's `Done` signal and names a method of a service the program holds. Done
// values that hold no service still flow, so the named method stays and the
// bundle runs. (Whether the unread method goes depends on the program-wide
// `Done` pool, an accepted imprecision, so it is not checked.)
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
