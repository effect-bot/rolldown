import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// Each attempt succeeds with the service. `Effect.repeat` provides
// `Schedule.CurrentMetadata` to the next attempt, whose `input` is that success.
const Store = Context.Service('test/RepeatMetadataInputStore');

let calls = 0;
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ metadataUsed: () => calls++, unusedMetadata: () => 'unused' }),
);

const action = Effect.gen(function* () {
  const metadata = yield* Schedule.CurrentMetadata;
  if (metadata.input !== undefined) metadata.input.metadataUsed();
  return yield* Store;
});

await Effect.runPromise(Effect.provide(Effect.repeat(action, { times: 1 }), StoreLayer));
assert.strictEqual(calls, 1);
