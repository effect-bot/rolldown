import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Effect.effectify is summarised from its types. Its callback is declared as a
// type parameter, `F extends (...args: Array<any>) => any`, and the effectified
// function hands its arguments to that callback. The callback's parameters are
// open, so the service arrives: `effectifyUsed` must stay, and the unread
// method is removed.
const Store = Context.Service('test/TypedConstraintStore');

const readStore = (store, callback) => callback(null, store.effectifyUsed());
const readEffect = Effect.effectify(readStore);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return yield* readEffect(store);
});

const layer = Layer.succeed(
  Store,
  Store.of({ effectifyUsed: () => 'used', effectifyUnused: () => 'unused' }),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
