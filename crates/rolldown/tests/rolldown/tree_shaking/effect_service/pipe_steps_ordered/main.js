import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Pipeable from 'effect/Pipeable';

// On a `Pipeable` value, `x.pipe(f, g, h)` is `h(g(f(x)))`: each step gets
// only the previous step's result. `h` enumerates its input, which is `g`'s
// plain row, not the service `f` returns.

const Store = Context.Service('test/RealPipeStepsOrdered');

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
  const keys = new Pipeable.Class().pipe(
    () => store,
    (service) => ({ read: service.readOne() }),
    (row) => Object.keys(row),
  );
  return keys;
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['read']);
