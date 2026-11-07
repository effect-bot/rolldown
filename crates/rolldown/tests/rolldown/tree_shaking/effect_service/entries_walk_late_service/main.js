import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A record that holds no service is walked by `Object.entries`; a service is
// then stored into it, and read back through a second walk. A walk of data
// with no service may stand for its values as opaque ones, but only while the
// data holds none: once the service is stored, the method read through the
// walk must stay.
const Store = Context.Service('test/EntriesLateStore');
const StoreLayer = Layer.succeed(Store, Store.of({ used: () => 'used' }));

const record = { label: 'box' };
const walk = () => Object.entries(record);

const program = Effect.gen(function* () {
  const before = walk().length;
  record.store = yield* Store;
  const [, store] = walk().find(([key]) => key === 'store');
  return `${before} ${store.used()}`;
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), '1 used');
