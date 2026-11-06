import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A service read out of the Context with `Context.get` by a static name: that
// name is demanded, other unread methods can still go.
const Store = Context.Service('test/RealEnumerationRead');

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const ping = () => 'pong';
    const enumUnused = () => 'unused';
    return Store.of({ readOne, ping, enumUnused });
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const context = yield* Effect.context();
  const pings = [];
  pings.push(Context.get(context, Store).ping());
  return [store.readOne(), pings];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'readOne',
  ['pong'],
]);
