import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { identity } from './late.js';

// An external module returns the key it is handed. The program looks the
// service up through that return value, so the lookup must reach Store's
// implementation: `identityUsed` is read there and must stay. Handing a key
// to unseen code bails nothing, so `identityUnused` still prunes, and Other,
// never handed over, prunes as usual.
const Store = Context.Service('test/ExternalIdentityStore');
const Other = Context.Service('test/ExternalIdentityOther');

const program = Effect.gen(function* () {
  const other = yield* Other;
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [Context.get(context, identity(Store)).identityUsed(), other.identityOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(Store, Store.of({ identityUsed: () => 'used', identityUnused: () => 'unused' })),
  Layer.succeed(
    Other,
    Other.of({ identityOtherUsed: () => 'other', identityOtherUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 'other']);
