import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/RealRefStore');
const refUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const refUnused = () => refUnusedHelper();
    return Store.of({ readOne, refUnused });
  }),
);
const name = ['ref', 'Unused'].join('');
// A program-declared Reference whose default holds an object the program
// later fills with its Context: the default is what `yield*` gives.
const holder = { context: undefined };
const Holder = Context.Reference('test/RealReferenceDefaultHolder', { defaultValue: () => holder });
const program = Effect.gen(function* () {
  holder.context = yield* Effect.context();
  const held = yield* Holder;
  const out = [];
  for (const service of held.context.mapUnsafe.values()) {
    if (typeof service[name] === 'function') out.push(service[name]());
  }
  return [(yield* Store).readOne(), out];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['r', ['u']]);
