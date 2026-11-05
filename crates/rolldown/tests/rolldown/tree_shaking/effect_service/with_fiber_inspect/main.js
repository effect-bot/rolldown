import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

import { inspect } from 'node:util';

// A `withFiber` closure formats the fiber's Context with `util.inspect`.
// Formatting reads only `[util.inspect.custom]` and `toJSON` on the entries,
// so `inspectUnused` is removed. The output lists fewer method names, which
// is accepted; the program only checks that the Context was printed.
const Store = Context.Service('test/RealWithFiberInspect');
const inspectUnusedHelper = () => 'unused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const inspectUnused = () => inspectUnusedHelper();
    return Store.of({ readOne, inspectUnused });
  }),
);

const describe = Effect.withFiber((fiber) => Effect.succeed(inspect(fiber.context, { depth: 4 })));

const program = Effect.gen(function* () {
  const store = yield* Store;
  const text = yield* describe;
  return [store.readOne(), text.includes('test/RealWithFiberInspect')];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'readOne',
  true,
]);
