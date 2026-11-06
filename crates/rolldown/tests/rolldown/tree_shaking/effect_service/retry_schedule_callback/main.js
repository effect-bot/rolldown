import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// `Effect.retry` steps its Schedule after each failure, so the `Schedule.map`
// callback runs and calls a method of the service it closes over.
const Store = Context.Service('test/RetryScheduleStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ onRetry: () => 'retried', unusedOnRetry: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const log = [];
  let attempts = 0;
  const result = yield* Effect.retry(
    Effect.suspend(() => (attempts++ === 0 ? Effect.fail('once') : Effect.succeed('ok'))),
    Schedule.map(Schedule.recurs(1), () => {
      log.push(store.onRetry());
    }),
  );
  return `${result} ${log.join(',')}`;
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'ok retried');
