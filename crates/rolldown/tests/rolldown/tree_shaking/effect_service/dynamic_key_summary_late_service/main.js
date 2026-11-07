import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';

// An `Option` is stored under a dynamic key while it holds no service, then
// the same call site builds one holding the service, stored through the same
// store and read back. The summary the dynamic-key store judged service-free
// gains the service later: the method read through it must stay.
const Store = Context.Service('test/DynamicKeySummaryStore');
const StoreLayer = Layer.succeed(Store, Store.of({ used: () => 'used' }));

const box = { current: 'none' };
const makeCell = () => Option.some([box.current]);
const target = {};
const key = JSON.parse('"cell"');
// One store for both writes (the returned closure keeps it from being cloned).
function put(value) {
  target[key] = value;
  return () => value;
}
put(makeCell());

const program = Effect.gen(function* () {
  box.current = yield* Store;
  put(makeCell());
  const cell = target[JSON.parse('"cell"')];
  return Option.getOrThrow(cell)[0].used();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'used');
