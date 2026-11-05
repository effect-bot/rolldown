import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Effect.updateServiceScoped` is a keyed lookup like `updateService`, here by
// a hand-made key with a constant string. Its callback reads `scopedRead`, so
// that method stays; `scopedUnused`, which nothing reads, goes.
const Store = Context.Service('test/RealUpdateServiceScoped');

const scopedUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const scopedRead = () => 'scoped';
    const scopedUnused = () => scopedUnusedHelper();
    return Store.of({ readOne, scopedRead, scopedUnused });
  }),
);

const program = Effect.scoped(
  Effect.gen(function* () {
    const store = yield* Store;
    let observed;
    yield* Effect.updateServiceScoped({ key: 'test/RealUpdateServiceScoped' }, (old) => {
      observed = old.scopedRead();
      return old;
    });
    return [store.readOne(), observed];
  }),
);

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'scoped',
]);
