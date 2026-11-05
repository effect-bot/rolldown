import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/RealRefplainStore');
const refPlainUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const refPlainUnused = () => refPlainUnusedHelper();
    return Store.of({ readOne, refPlainUnused });
  }),
);
// A program-declared Reference whose default holds data only: enumerating it
// reads no service.
const Settings = Context.Reference('test/RealReferenceDefaultPlain', {
  defaultValue: () => ({ mode: 'fast', retries: 2 }),
});
const program = Effect.gen(function* () {
  const settings = yield* Settings;
  return [(yield* Store).readOne(), Object.keys(settings).map((key) => `${key}=${settings[key]}`)];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'r',
  ['mode=fast', 'retries=2'],
]);
