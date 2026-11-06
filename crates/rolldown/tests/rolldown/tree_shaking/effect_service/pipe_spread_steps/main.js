import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Pipeable from 'effect/Pipeable';

// On a `Pipeable` value, `x.pipe(...steps)` runs the steps in turn, so the
// second gets the service the first returns and calls `writeOne` on it.

const Store = Context.Service('test/RealPipeSpreadSteps');

const readHelper = () => 'read';
const writeHelper = () => 'write';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => readHelper();
    const writeOne = () => writeHelper();
    return Store.of({ readOne, writeOne });
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const steps = [() => store, (service) => service.writeOne()];
  return [store.readOne(), new Pipeable.Class().pipe(...steps)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'write',
]);
