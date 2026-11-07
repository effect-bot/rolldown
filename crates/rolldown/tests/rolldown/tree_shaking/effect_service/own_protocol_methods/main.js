import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// An array and a plain object override protocol methods (`toJSON`, `toString`)
// that read the service. Host serialization and string conversion call them,
// and so does Effect's formatting of a log message: each read method stays,
// while the unread one is removed.
const Store = Context.Service('test/OwnProtocolStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    describe: () => 'described',
    label: () => 'labelled',
    format: () => 'formatted',
    ownProtocolUnused: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const list = ['a'];
  list.toJSON = () => store.describe();
  const named = {
    toString() {
      return store.label();
    },
  };
  const logged = {
    toJSON() {
      return store.format();
    },
  };
  yield* Effect.log(logged);
  return `${JSON.stringify(list)} ${String(named)}`;
});

assert.strictEqual(
  await Effect.runPromise(Effect.provide(program, StoreLayer)),
  '"described" labelled',
);
