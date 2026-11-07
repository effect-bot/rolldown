import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';

// The service is wrapped in an Option outside any class body, and the Option
// is stored in a field of a class instance. Nothing reads a method of the
// service, but a value holding it sits in a class instance's field, so its key
// bails and every method stays.
class Holder {}

const Store = Context.Service('test/ClassFieldTypeModelledStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ heldFirst: () => 'first', heldSecond: () => 'second' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const holder = new Holder();
  holder.held = Option.some(store);
  return Option.isSome(holder.held);
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), true);
