import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { identity } from './late.js';

// The program hands a key name only known at run time to an external module,
// which hands it straight back, and looks the service up by what comes back.
// Tim's decision (2026-11-09): a runtime name string handed to code the pass
// can't see, or doesn't follow, and returned by it is not read as a name of any
// key, as for strings that code makes itself. The lookup keeps nothing, so both
// method definitions go. Canonical keys and literal key names handed over and
// returned still reach their services (external_identity_key,
// external_returned_key_held); a runtime name in a structural key's `key` field
// is excluded too (external_returned_structural_key).
const Store = Context.Service('test/ExternalReturnedRuntimeName');

const program = Effect.gen(function* () {
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return Context.getOrUndefinedUnsafe(context, identity(process.env.EXTERNAL_RETURNED_NAME)).used();
});

const layer = Layer.succeed(Store, Store.of({ used: () => 'used', unused: () => 'unused' }));
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
