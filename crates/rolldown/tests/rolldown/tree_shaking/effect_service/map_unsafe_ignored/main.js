import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { hold } from './late.js';

// `Context.mapUnsafe` is ignored: reading a service out of it, enumerating
// it, writing through it and handing it to code the pass can't see bail
// nothing and keep nothing. Only `readOne` is read outside the view.
const Store = Context.Service('test/MapUnsafeIgnored');
const viewUnusedHelper = () => 'u';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    readOne: () => 'r',
    viewRead: () => 'v',
    viewComputed: () => 'c',
    viewUnused: () => viewUnusedHelper(),
  }),
);

const name = String(process.env.EFFECT_FIXTURE_METHOD ?? 'viewComputed');

const program = Effect.gen(function* () {
  const store = yield* Store;
  const view = (yield* Effect.context()).mapUnsafe;
  const read = view.get(Store.key).viewRead();
  const computed = [];
  for (const service of view.values()) {
    if (typeof service[name] === 'function') computed.push(service[name]());
  }
  view.set('test/MapUnsafeIgnoredCopy', store);
  hold(view);
  return [store.readOne(), read, computed];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'r',
  'v',
  ['c'],
]);
