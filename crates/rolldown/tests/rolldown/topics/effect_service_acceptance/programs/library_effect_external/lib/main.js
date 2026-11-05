import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A library bundled with `effect` external. Every key and Layer is exported, so
// a consumer may read any method: nothing may be removed, and the output must
// behave identically for every consumer.
export class Store extends Context.Service()('lib/Store') {}
export class Counter extends Context.Service()('lib/Counter') {}

const storeReadHelper = (key) => `store:${key}`;
const storeWriteHelper = (key, value) => `wrote ${key}=${value}`;

export const StoreLive = Layer.succeed(Store, {
  read: (key) => storeReadHelper(key),
  write: (key, value) => storeWriteHelper(key, value),
  keys: () => ['a', 'b'],
});

export const CounterLive = Layer.effect(
  Counter,
  Effect.gen(function* () {
    let n = 0;
    const increment = () => ++n;
    const current = () => n;
    const reset = () => {
      n = 0;
      return 'reset';
    };
    return Counter.of({ increment, current, reset });
  }),
);

export const AppLive = Layer.mergeAll(StoreLive, CounterLive);

export const describe = Effect.gen(function* () {
  const store = yield* Store;
  const counter = yield* Counter;
  counter.increment();
  return `${store.read('x')} after ${counter.current()} increments`;
});
