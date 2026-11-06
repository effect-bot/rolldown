import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// The Schedule outputs the service. `Effect.retry` provides
// `Schedule.CurrentMetadata` to the retry, whose `output` is that output.
const Store = Context.Service('test/RetryMetadataOutputStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ outputUsed: () => 'from metadata', unusedOutput: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const action = Effect.gen(function* () {
    const metadata = yield* Schedule.CurrentMetadata;
    if (metadata.attempt === 0) return yield* Effect.fail('first');
    return metadata.output.outputUsed();
  });
  return yield* Effect.retry(
    action,
    Schedule.map(Schedule.recurs(1), () => store),
  );
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'from metadata');
