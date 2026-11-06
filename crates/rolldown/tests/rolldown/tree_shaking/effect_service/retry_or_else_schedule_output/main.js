import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// The Schedule outputs the service, and `orElse` gets that output as its
// second argument once the Schedule is exhausted.
const Store = Context.Service('test/RetryOrElseOutputStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readOut: () => 'from output', unusedOut: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return yield* Effect.retryOrElse(
    Effect.fail('always'),
    Schedule.map(Schedule.recurs(1), () => store),
    (_error, out) => Effect.succeed(out.readOut()),
  );
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'from output');
