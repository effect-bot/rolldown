import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Fiber from 'effect/Fiber';
import * as Layer from 'effect/Layer';

// The child fiber fails with the service. `Fiber.join` fails with the same
// error, so the service reaches the program through the failure channel.
const Store = Context.Service('test/FiberJoinFailureStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readFailed: () => 'failed', unusedFailed: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const fiber = yield* Effect.forkChild(Effect.flatMap(Effect.service(Store), Effect.fail));
  return yield* Effect.catch(Fiber.join(fiber), (store) => Effect.succeed(store.readFailed()));
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'failed');
