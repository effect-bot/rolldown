import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { greeting } from './late.js';

// An external module builds an Effect with the Effect primitive it is handed,
// and the program runs it next to its own service. Code the pass cannot see is
// assumed not to reach services through the runtime, so nothing bails.
const Store = Context.Service('test/ExternalEffectRunStore');
const externalRunUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readStore: () => 'store', externalRunUnused: () => externalRunUnusedHelper() }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const hello = yield* greeting(Effect.sync);
  return [store.readStore(), hello];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'store',
  'hello',
]);
