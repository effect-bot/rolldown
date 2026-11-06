import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Equal from 'effect/Equal';
import * as Layer from 'effect/Layer';

// One method body serves every receiver. Like effect's `Hash.cached(this, () => ...)`,
// it closes over `this`, so it is not cloned per call site, and its helper receives
// `this` directly: `get` returns the Table service on one receiver, the Store on the other.
const pick = (entry) => entry.value;
const cached = (self, compute) => compute(self);
const proto = {
  get() {
    return cached(this, () => pick(this));
  },
};

const tableRead = () => 'table read';
const tableWrite = () => 'table write';
const Table = Context.Service('test/ReceiverSeparationTable');
const Store = Context.Service('test/ReceiverSeparationStore');
const layer = Layer.merge(
  Layer.succeed(Table, Table.of({ tableRead, tableWrite })),
  Layer.succeed(Store, Store.of({ receiverUsed: () => 'used', unusedReceiver: () => 'unused' })),
);

const program = Effect.gen(function* () {
  const table = { __proto__: proto, value: yield* Table };
  const store = { __proto__: proto, value: yield* Store };
  // Structural equality reads every property of the Table service.
  return [Equal.equals(table.get(), { tableRead, tableWrite }), store.get().receiverUsed()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [true, 'used']);
