import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';

// An error failed with Effect.fail holds a function bound to the service. It is
// read back through the Exit's Cause (Effect.exit, Exit.isFailure,
// Cause.findError, Result.isSuccess) and called: the method it reads through
// `this` stays; the method nothing reads is removed.
const Store = Context.Service('test/ExitCauseBoundMember');
const boundUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store)({
  viaExitBound: () => 'bound',
  boundUnused: () => boundUnusedHelper(),
});

function readBound() {
  return this.viaExitBound();
}

const program = Effect.gen(function* () {
  const store = yield* Store;
  const exit = yield* Effect.exit(Effect.fail({ read: readBound.bind(store) }));
  if (Exit.isFailure(exit)) {
    const found = Cause.findError(exit.cause);
    if (Result.isSuccess(found)) {
      return found.success.read();
    }
  }
  return 'missing';
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'bound');
