import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The Function constructor read through `Reflect.get`: bare, on
// `globalThis.Reflect` and through an alias of it, and with a spread that moves
// the literal name to another argument position. The compiled code receives no
// service and is not run as an Effect, so the unread method is removed.
const Store = Context.Service('test/RealLoaderReflectGet');

const reflectGetUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const reflectGetUnused = () => reflectGetUnusedHelper();
    return Store.of({ readOne, reflectGetUnused });
  }),
);

const load = Reflect.get(() => 0, 'constructor');
const loadGlobal = globalThis.Reflect.get(() => 0, 'constructor');
const reflect = globalThis.Reflect;
const loadAlias = reflect.get(() => 0, 'constructor');
const loadSpread = Reflect.get(...[], () => 0, 'constructor');

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [
    store.readOne(),
    load('return 1')(),
    loadGlobal('return 2')(),
    loadAlias('return 3')(),
    loadSpread('return 4')(),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  1,
  2,
  3,
  4,
]);
