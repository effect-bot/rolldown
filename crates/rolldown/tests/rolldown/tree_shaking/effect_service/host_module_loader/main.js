import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { getBuiltinModule } from 'node:process';

// A Context is handed to code compiled at run time by the host. The pass cannot
// see that code, so every entry of the Context keeps every method.
const Store = Context.Service('test/RealHostModuleLoader');
const hostLoaderUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const hostLoaderUnused = () => hostLoaderUnusedHelper();
    return Store.of({ readOne, hostLoaderUnused });
  }),
);

const observe = (context) =>
  getBuiltinModule('vm').compileFunction(
    'return ctx.mapUnsafe.get("test/RealHostModuleLoader").hostLoaderUnused()',
    ['ctx'],
  )(context);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [store.readOne(), observe(context)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'unused',
]);
