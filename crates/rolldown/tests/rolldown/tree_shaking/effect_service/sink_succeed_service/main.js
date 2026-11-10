import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Sink from 'effect/Sink';
import * as Stream from 'effect/Stream';
// `Sink.succeed(store)` ends with the service as an ordinary successful result,
// and `Stream.run` succeeds with it. A Sink's result is not a `Done` value, so
// the method the program calls on it stays and the bundle runs.
const Store = Context.Service('test/SinkSucceedService');
const program = Effect.gen(function* () {
  const store = yield* Store;
  const returned = yield* Stream.run(Stream.empty, Sink.succeed(store));
  return returned.sinkUsed();
});
assert.strictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.succeed(Store)({ sinkUsed: () => 'used', sinkUnused: () => 'unused' }),
    ),
  ),
  'used',
);
