import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A hand-made key object with the same key string looks the service up without
// the declared key. Every method of the service must be kept.
const Store = Context.Service('test/RealForgedKey');
const forgedUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const forgedUnused = () => forgedUnusedHelper();
    return Store.of({ readOne, forgedUnused });
  }),
);

const observe = (context) => Context.get(context, { key: 'test/RealForgedKey' }).forgedUnused();

const program = Effect.gen(function* () {
  const store = yield* Store;
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [store.readOne(), observe(context)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'unused',
]);
