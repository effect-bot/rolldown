import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schema from 'effect/Schema';
import * as SchemaTransformation from 'effect/SchemaTransformation';

// With `concurrency`, Schema runs the transformation's decode on forked fibers
// (the internal `iterateConcurrent`). The decode yields the service and returns
// it as the decoded value, so the service reaches the program through Schema's
// runtime code, and the method the program calls on it must stay.
const Store = Context.Service('test/SchemaServiceValueStore');

const StoreLayer = Layer.succeed(Store, Store.of({ used: () => 'used' }));

const WithStore = Schema.String.pipe(
  Schema.decodeTo(
    Schema.Any,
    SchemaTransformation.transformEffect({
      decode: (s) =>
        Effect.gen(function* () {
          const store = yield* Store;
          return { name: s, store };
        }),
      encode: (value) => Effect.succeed(value.name),
    }),
  ),
);

const Item = Schema.Struct({ first: WithStore, second: WithStore });

const program = Effect.gen(function* () {
  const item = yield* Schema.decodeUnknownEffect(Item)(
    { first: 'a', second: 'b' },
    { concurrency: 'unbounded' },
  );
  return `${item.first.name}${item.second.name} ${item.second.store.used()}`;
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'ab used');
