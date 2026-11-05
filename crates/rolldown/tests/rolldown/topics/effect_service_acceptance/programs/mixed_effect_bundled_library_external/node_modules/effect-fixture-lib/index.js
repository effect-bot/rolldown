import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// An Effect library: a key, its Layer and an effect that reads the service.
export class Store extends Context.Service()('fixture-lib/Store') {}

const countHelper = (text) => text.split(/\s+/).filter(Boolean).length;
const libUnusedHelper = (text) => text.toUpperCase();

export const StoreLive = Layer.succeed(Store, {
  count: (text) => countHelper(text),
  libUnused: (text) => libUnusedHelper(text),
});

export const countWords = (text) =>
  Effect.gen(function* () {
    const store = yield* Store;
    return store.count(text);
  });
