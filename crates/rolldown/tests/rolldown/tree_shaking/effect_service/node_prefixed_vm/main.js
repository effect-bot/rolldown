import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { compileFunction } from 'node:vm';

// Retention: the fiber's Context is handed to code compiled by `node:vm`, which
// reads a service by key string and calls a method. A Context handed to code the
// pass cannot see bails its entries, so `vmUnused` stays.
const Store = Context.Service('test/RealNodeVm');

const vmUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const vmUnused = () => vmUnusedHelper();
    return Store.of({ readOne, vmUnused });
  }),
);

const observe = (context) =>
  compileFunction('return ctx.mapUnsafe.get("test/RealNodeVm").vmUnused()', ['ctx'])(context);

const program = Effect.gen(function* () {
  const store = yield* Store;
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [store.readOne(), observe(context)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'unused',
]);
