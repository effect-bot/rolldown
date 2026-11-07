import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Accessors reached through their property descriptors: a getter returning the
// service is called through `Object.getOwnPropertyDescriptors(...).item.get`
// and `Object.getOwnPropertyDescriptor(...).get`, and a setter is handed the
// service through `.set`. Each method read on what they give back stays.
const Store = Context.Service('test/DescriptorAccessorStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ used: () => 'used', single: () => 'single', viaSetter: () => 'set' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const source = {
    get item() {
      return store;
    },
  };
  const getter = Object.getOwnPropertyDescriptors(source).item.get;
  const fromGetter = getter.call(source).used();
  const single = Object.getOwnPropertyDescriptor(source, 'item').get.call(source).single();
  let held;
  const sink = {
    set item(value) {
      held = value;
    },
  };
  Object.getOwnPropertyDescriptors(sink).item.set.call(sink, store);
  return `${fromGetter} ${single} ${held.viaSetter()}`;
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'used single set');
