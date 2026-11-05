import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class-form key that overrides the inherited `key`: its services live under
// the static string, not the one in the heritage, so a key declared with that
// string reads the same service.
class Store extends Context.Service()('test/RealKeyClassDeclaredIdentity') {
  static key = 'test/RealKeyClassActualIdentity';
}
const Alias = Context.Service('test/RealKeyClassActualIdentity');
const overridesKeyUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    return Store.of({
      readOne: () => 'read',
      overridesKeyUnused: () => overridesKeyUnusedHelper(),
    });
  }),
);

const program = Effect.gen(function* () {
  const alias = yield* Alias;
  return alias.overridesKeyUnused();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'unused');
