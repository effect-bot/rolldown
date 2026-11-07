import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';

// A service failed into a Cause with Cause.fail and read back, typed as the
// Cause's error: once through Effect.catch, once through Cause.findError.
// Each path calls its own method, which stays; the unread method goes.
const Store = Context.Service('test/CauseReadBack');
const causeUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store, {
  readCaught: () => 'caught',
  readFound: () => 'found',
  causeUnused: () => causeUnusedHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  const caught = yield* Effect.failCause(Cause.fail(store)).pipe(
    Effect.catch((error) => Effect.succeed(error.readCaught())),
  );
  const found = Cause.findError(Cause.fail(store));
  return [caught, Result.isSuccess(found) ? found.success.readFound() : 'missing'];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'caught',
  'found',
]);
