import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as KeyName from './key.js';

// The namespace of key.js has a `key` property: a lookup through it finds the
// service whose key string key.js exports.
const Store = Context.Service('test/NamespaceKeyLookup');
const viaNamespaceHelper = () => 'namespace';
const storeUnreadHelper = () => 'unread';

const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    readOne: () => 'read',
    viaNamespace: () => viaNamespaceHelper(),
    storeUnread: () => storeUnreadHelper(),
  }),
);

const observe = (context) => Context.get(context, KeyName).viaNamespace();

const program = Effect.gen(function* () {
  const store = yield* Store;
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [store.readOne(), observe(context)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'namespace',
]);
