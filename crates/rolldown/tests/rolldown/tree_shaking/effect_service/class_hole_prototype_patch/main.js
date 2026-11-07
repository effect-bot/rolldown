import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Code outside the class installs a method on its prototype, and the program
// calls it through an instance. The method is not class code: it runs, the
// service method it reads stays, the unread one goes, and Store does not bail.
const Store = Context.Service('test/ClassHolePrototypePatchStore');
class Target {}

const program = Effect.gen(function* () {
  const store = yield* Store;
  Target.prototype.extra = function () {
    return store.patchUsed();
  };
  return new Target().extra();
});

const layer = Layer.succeed(
  Store,
  Store.of({ patchUsed: () => 'used', patchUnused: () => 'unused' }),
);
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
