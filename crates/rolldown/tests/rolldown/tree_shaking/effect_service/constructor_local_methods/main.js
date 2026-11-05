import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Base case: a provider built by `Layer.effect` + `Effect.gen` whose methods
// are constructor-local consts, consumed with `yield*`. Only `readOne` is read.
const Store = Context.Service('test/ConstructorLocalStore');

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
  return store.readOne();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'read');
