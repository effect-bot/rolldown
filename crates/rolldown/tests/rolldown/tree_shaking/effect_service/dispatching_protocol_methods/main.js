import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A function's own protocol methods are host prototype methods that call the
// function: `toJSON` is `Function.prototype.call`, `toString` is the
// function's own `call`. JSON serialization and string conversion invoke
// them, and they invoke the function, which reads the service. That dispatch
// is the only read of each method: both stay, while the unread one is removed.
const Store = Context.Service('test/DispatchingProtocolStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    serialized: () => 'serialized',
    converted: () => 'converted',
    dispatchingUnused: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  function serialize() {
    return store.serialized();
  }
  serialize.toJSON = Function.prototype.call;
  function convert() {
    return store.converted();
  }
  convert.toString = convert.call;
  return `${JSON.stringify(serialize)} ${String(convert)}`;
});

assert.strictEqual(
  await Effect.runPromise(Effect.provide(program, StoreLayer)),
  '"serialized" converted',
);
