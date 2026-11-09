import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { identity } from './late.js';

// The program hands a key name only known at run time to an external module,
// which hands it straight back. The test sets the variable to the service's key
// name, so the lookup by what comes back reaches the service: the method read
// there must stay. The unread method still prunes.
const Store = Context.Service('test/ExternalReturnedRuntimeName');

const program = Effect.gen(function* () {
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return Context.getOrUndefinedUnsafe(context, identity(process.env.EXTERNAL_RETURNED_NAME)).used();
});

const layer = Layer.succeed(Store, Store.of({ used: () => 'used', unused: () => 'unused' }));
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
