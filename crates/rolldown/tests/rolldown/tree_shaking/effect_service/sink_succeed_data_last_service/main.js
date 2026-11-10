import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Sink from 'effect/Sink';
import * as Stream from 'effect/Stream';
// `Stream.empty.pipe(Stream.run(Sink.succeed(store)))`, the data-last form,
// succeeds with the service as the Sink's result. No service passes through a
// Sink's result or leftovers (Tim's decision, assumption 6), so the read keeps
// nothing: both method definitions are removed and the bundle is not run.
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
