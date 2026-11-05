import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The entry exports a function that runs the importer's callback against the
// library's service, so the callback may read any of its methods. The service
// of the library's internal run is never handed out: its unread method can go.
const Store = Context.Service('test/RealLibraryStore');
const Internal = Context.Service('test/RealLibraryInternal');
const libraryStoreUnusedHelper = () => 's2';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readStore = () => 's1';
    const libraryStoreUnused = () => libraryStoreUnusedHelper();
    return Store.of({ readStore, libraryStoreUnused });
  }),
);
const libraryInternalUnusedHelper = () => 'i2';
const InternalLayer = Layer.effect(
  Internal,
  Effect.gen(function* () {
    const readInternal = () => 'i1';
    const libraryInternalUnused = () => libraryInternalUnusedHelper();
    return Internal.of({ readInternal, libraryInternalUnused });
  }),
);

export const withStore = (f) =>
  Effect.runPromise(
    Effect.provide(
      Effect.gen(function* () {
        return f(yield* Store);
      }),
      StoreLayer,
    ),
  );

const internal = Effect.gen(function* () {
  return (yield* Internal).readInternal();
});
assert.strictEqual(await Effect.runPromise(Effect.provide(internal, InternalLayer)), 'i1');
