import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Sink from 'effect/Sink';
import * as Stream from 'effect/Stream';
// `Sink.succeed(store)` handed to the data-last `Stream.run` through `pipe`
// ends with the service as an ordinary successful result. Tim's exclusion of
// stream elements collected into a Sink's result does not cover a value the
// Sink is handed itself, so the method the program calls on it stays and the
// bundle runs.
const Store = Context.Service('test/SinkSucceedDataLastService');
const program = Effect.gen(function* () {
  const store = yield* Store;
  const returned = yield* Stream.empty.pipe(Stream.run(Sink.succeed(store)));
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
