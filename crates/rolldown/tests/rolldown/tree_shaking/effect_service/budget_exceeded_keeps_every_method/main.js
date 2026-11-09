import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/BudgetStore');

const program = Effect.gen(function* () {
  return (yield* Store).budgetUsed();
});

const layer = Layer.succeed(
  Store,
  Store.of({ budgetUsed: () => 'used', budgetUnused: () => 'unused' }),
);
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
