import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// User code enumerates, indexes by non-constant names and serializes its own
// data, never a service or Context. The pass must still remove `writeOne`.
const Store = Context.Service('test/RealCleanData');

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

const settings = { mode: 'fast', level: 2 };
const describe = () => {
  const names = Object.keys(settings);
  const values = [];
  for (const name in settings) values.push(settings[name]);
  const copy = structuredClone(settings);
  return [names.join(','), values.join(','), Reflect.ownKeys(copy).length, JSON.stringify(copy)];
};
const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), ...describe()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'mode,level',
  'fast,2',
  2,
  '{"mode":"fast","level":2}',
]);
