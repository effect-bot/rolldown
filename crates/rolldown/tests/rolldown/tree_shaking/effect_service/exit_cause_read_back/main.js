import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';

// A service failed with Effect.fail and read back through the Exit's Cause:
// Effect.exit, narrowed with Exit.isFailure, then Cause.findError narrowed
// with Result.isSuccess. The method read back calls a sibling through `this`,
// so the sibling stays; the method nothing reads is removed.
const Store = Context.Service('test/ExitCauseReadBack');
const exitUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store)({
  readExit() {
    return 'e:' + this.exitHelper();
  },
  exitHelper: () => 'h',
  exitUnused: () => exitUnusedHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  const exit = yield* Effect.exit(Effect.fail(store));
  if (Exit.isFailure(exit)) {
    const found = Cause.findError(exit.cause);
    if (Result.isSuccess(found)) {
      return found.success.readExit();
    }
  }
  return 'missing';
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'e:h');
