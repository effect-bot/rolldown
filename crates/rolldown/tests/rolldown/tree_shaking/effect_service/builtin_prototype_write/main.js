import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Replacing Map.prototype.get captures the service map of a Context when Effect
// looks a service up. Every method of the service must be kept.
const Store = Context.Service('test/RealPrototypeWrite');
const prototypeWriteUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const prototypeWriteUnused = () => prototypeWriteUnusedHelper();
    return Store.of({ readOne, prototypeWriteUnused });
  }),
);

const maps = [];
const originalGet = Map.prototype.get;
Map.prototype.get = function (key) {
  maps.push(this);
  return originalGet.call(this, key);
};

const observe = () => {
  Map.prototype.get = originalGet;
  const map = maps.find((map) => map.has('test/RealPrototypeWrite'));
  return originalGet.call(map, 'test/RealPrototypeWrite').prototypeWriteUnused();
};

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), observe()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'unused',
]);
