import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A span never carries services (the issue's span decision): the service stored
// as a span attribute is not tracked, so it keeps nothing. The `useSpan`
// callback still runs, and it calls a method of the service it closes over:
// that method stays and the bundle runs; the method nothing calls is removed.
const Store = Context.Service('test/UseSpanCallback');
const spanUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store)({
  inSpan: () => 'in span',
  spanUnused: () => spanUnusedHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  return yield* Effect.useSpan('test/use-span-callback', (span) => {
    span.attribute('store', store);
    return Effect.sync(() => store.inSpan());
  });
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'in span');
