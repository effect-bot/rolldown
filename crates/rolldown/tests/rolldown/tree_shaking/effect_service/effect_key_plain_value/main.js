import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as References from 'effect/References';

const Store = Context.Service('test/RealPlainkeyStore');
const plainKeyUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const plainKeyUnused = () => plainKeyUnusedHelper();
    return Store.of({ readOne, plainKeyUnused });
  }),
);
const name = ['plainKey', 'Unused'].join('');
// A service of a key Effect's own modules declare, holding a string, is
// converted to a string: no runtime object is read.
const program = Effect.gen(function* () {
  const level = yield* References.MinimumLogLevel;
  return [(yield* Store).readOne(), `level ${level}`];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'r',
  'level Info',
]);
