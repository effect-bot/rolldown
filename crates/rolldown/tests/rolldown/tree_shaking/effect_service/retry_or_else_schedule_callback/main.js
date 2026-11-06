import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// `Effect.retryOrElse` steps its Schedule after each failure, so the
// `Schedule.map` callback runs and calls a method of the service it closes over.
const Store = Context.Service('test/RetryOrElseCallbackStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ onRetry: () => 'retried', unusedOnRetry: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const log = [];
  return yield* Effect.retryOrElse(
    Effect.fail('always'),
    Schedule.map(Schedule.recurs(1), () => {
      log.push(store.onRetry());
    }),
    (error) => Effect.succeed(`${error} ${log[0]}`),
  );
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'always retried');
