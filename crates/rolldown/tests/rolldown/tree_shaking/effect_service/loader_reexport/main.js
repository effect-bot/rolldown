import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { compile } from './loader.js';

// A code loader reached through a renamed re-export of `node:vm` compiles code
// outside the bundle. It receives no service, so the unread method is removed.
const Store = Context.Service('test/RealLoaderReexport');

const reexportLoaderUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const reexportLoaderUnused = () => reexportLoaderUnusedHelper();
    return Store.of({ readOne, reexportLoaderUnused });
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), compile('return 1')()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['read', 1]);
