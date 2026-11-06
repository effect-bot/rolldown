import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// The source dies with the service. `Effect.retryOrElse` retries only typed
// failures, so the defect skips the Schedule and `orElse` and reaches the
// `Effect.catchDefect` handler.
const Store = Context.Service('test/RetryOrElseDefectStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ defectUsed: () => 'defect', unusedDefect: () => 'unused' }),
);

const program = Effect.catchDefect(
  Effect.retryOrElse(Effect.flatMap(Effect.service(Store), Effect.die), Schedule.recurs(0), () =>
    Effect.succeed('fallback'),
  ),
  (defect) => Effect.succeed(defect.defectUsed()),
);

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'defect');
