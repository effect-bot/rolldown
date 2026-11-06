import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Two `Effect.all` results are mapped with the same builtin callback, `Object`.
// One holds the service, the other plain data that is enumerated. The builtin
// runs once per wrap, so the enumeration does not reach the service.

const Store = Context.Service('test/RealWrapCallbackSiteIsolation');

const readHelper = () => 'read';
const writeHelper = () => 'write';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => readHelper();
    const writeOne = () => writeHelper();
    return Store.of({ readOne, writeOne });
  }),
);

const program = Effect.gen(function* () {
  const services = yield* Effect.all([Effect.service(Store)]);
  const [store] = services.map(Object);
  const data = yield* Effect.all([Effect.succeed({ id: 1 })]);
  const rows = data.map(Object);
  return [store.readOne(), Object.keys(rows).join(',')];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['read', '0']);
