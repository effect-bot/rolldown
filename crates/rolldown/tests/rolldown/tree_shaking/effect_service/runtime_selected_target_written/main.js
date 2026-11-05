import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: the handler table also gets a field written under a name built
// from constants, so the candidates include that function and its read. Every
// method it can reach stays.
const Store = Context.Service('test/RealSelectedTargetWritten');
const writtenUnusedHelper = () => 'c';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readFirst = () => 'a';
    const writtenUnused = () => writtenUnusedHelper();
    return Store.of({ readFirst, writtenUnused });
  }),
);

const handlers = {
  first: (store) => store.readFirst(),
};
handlers[['thi', 'rd'].join('')] = (store) => store.writtenUnused();
const names = String(process.env.EFFECT_FIXTURE_HANDLERS ?? 'first third').split(' ');

const program = Effect.gen(function* () {
  const store = yield* Store;
  return names.map((name) => handlers[name](store));
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['a', 'c']);
