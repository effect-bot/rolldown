import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Context.makeUnsafe` builds a Context from a map keyed by strings. With
// literal keys the entries are known, and nothing reads `makeUnsafeUnused`.
const Store = Context.Service('test/RealMakeUnsafe');
const Made = Context.Service('test/RealMakeUnsafeMade');

const makeUnsafeUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const makeUnsafeUnused = () => makeUnsafeUnusedHelper();
    return Store.of({ readOne, makeUnsafeUnused });
  }),
);

const made = Context.makeUnsafe(new Map([['test/RealMakeUnsafeMade', { readMade: () => 'made' }]]));

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [
    store.readOne(),
    Context.get(made, Made).readMade(),
    typeof Context.makeUnsafe(new Map()),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'made',
  'object',
]);
