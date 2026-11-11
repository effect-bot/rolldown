import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { Store, table } from './lib.js';

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.budgetUsed(), Object.keys(table).length];
});

const layer = Layer.succeed(
  Store,
  Store.of({ budgetUsed: () => 'used', budgetUnread: () => 'unread' }),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 2000]);
