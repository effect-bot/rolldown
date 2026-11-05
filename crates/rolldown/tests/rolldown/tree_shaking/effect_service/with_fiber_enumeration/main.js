import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Effect.withFiber` hands its closure the running fiber. The closure
// enumerates the fiber's Context and calls `fiberPing` on every service that
// has it. `fiberPing` is demanded of every service; other unread methods go.
const Store = Context.Service('test/RealWithFiberEnumeration');
const fiberUnusedHelper = () => 'unused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const fiberPing = () => 'pong';
    const fiberUnused = () => fiberUnusedHelper();
    return Store.of({ readOne, fiberPing, fiberUnused });
  }),
);

const pings = Effect.withFiber((fiber) => {
  const out = [];
  for (const service of fiber.context.mapUnsafe.values()) {
    if (typeof service.fiberPing === 'function') out.push(service.fiberPing());
  }
  return Effect.succeed(out);
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), yield* pings];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'readOne',
  ['pong'],
]);
