import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A plain class whose static `key` happens to equal a provided service's key.
// It is not a key: only classes built from `Context.Service` or
// `Context.Reference` are, so handing it to Effect never looks the service up.
// `Effect.succeedSome` just wraps it, and the program reads every value of the
// class back. Only `storeUsed` is called on the service; nothing reads
// `storeUnused`.
const Store = Context.Service('test/PlainClassKeyCollision');
class Label {
  static key = 'test/PlainClassKeyCollision';
}
const storeUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store, {
  storeUsed: () => 'used',
  storeUnused: () => storeUnusedHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  const label = yield* Effect.succeedSome(Label);
  return [store.storeUsed(), label._tag === 'Some' ? Object.values(label.value) : []];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'used',
  ['test/PlainClassKeyCollision'],
]);
