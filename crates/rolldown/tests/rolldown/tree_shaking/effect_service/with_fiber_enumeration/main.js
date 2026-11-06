import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Effect.withFiber` hands its closure the running fiber. The closure
// reads Store out of the fiber's Context with `Context.get` and calls
// `fiberPing`. `fiberPing` is demanded; other unread methods go.
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
  out.push(Context.get(fiber.context, Store).fiberPing());
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
