import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Positive gate: data parsed with JSON.parse inside Effect.gen, passed through
// Effect.map and `yield*`, is user data. Enumerating it must not keep `writeOne`.
const Store = Context.Service('test/RealSuccessChannel');

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

const load = Effect.gen(function* () {
  return JSON.parse('{"mode":"fast","level":2}');
});
const program = Effect.gen(function* () {
  const store = yield* Store;
  const settings = yield* Effect.map(load, (parsed) => ({ ...parsed, extra: true }));
  const values = [];
  for (const name in settings) values.push(String(settings[name]));
  return [store.readOne(), Object.keys(settings).join(','), values.join(',')];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'mode,level,extra',
  'fast,2,true',
]);
