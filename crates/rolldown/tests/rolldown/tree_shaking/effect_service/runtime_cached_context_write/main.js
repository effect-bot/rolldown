import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as ManagedRuntime from 'effect/ManagedRuntime';

// `cachedContext` is a public, writable member of ManagedRuntime. The
// runtime's own Layer is never built: the program writes a Context to
// `cachedContext`, and every run uses that Context instead. The write
// provides Store, so the method the run reads must stay.
const Store = Context.Service('test/RuntimeCachedContext');
const cachedUnusedHelper = () => 'u';
const runtime = ManagedRuntime.make(Layer.effect(Store, Effect.die('not built')));
runtime.cachedContext = Context.make(
  Store,
  Store.of({ cachedUsed: () => 'cached', cachedUnused: () => cachedUnusedHelper() }),
);

const program = Effect.gen(function* () {
  return (yield* Store).cachedUsed();
});

assert.strictEqual(await runtime.runPromise(program), 'cached');
await runtime.dispose();
