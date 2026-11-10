import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Sink from 'effect/Sink';
import * as Stream from 'effect/Stream';
// A service collected by `Sink.takeWhile` comes back in the Sink's result. No
// service passes through a Sink's result or leftovers (Tim's decision,
// assumption 6), so the read keeps nothing: both method definitions are
// removed and the bundle is not run.
const Store = Context.Service('test/SinkTakeWhileService');
const program = Effect.gen(function* () {
  const store = yield* Store;
  const taken = yield* Stream.make(store).pipe(Stream.run(Sink.takeWhile(() => true)));
  return taken.map((s) => s.takenRead());
});
assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.succeed(Store)({ takenRead: () => 'read', takenUnused: () => 'unused' }),
    ),
  ),
  ['read'],
);
