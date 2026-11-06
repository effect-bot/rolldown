import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schema from 'effect/Schema';

// Decoding with `concurrency` runs Schema's internal helpers
// (`resolveConcurrency`, `iterateEager` for the Union, `iterateConcurrent` for
// the Struct's properties). They are plain JavaScript over the input: no Fiber
// or Context reaches the decoded value, so the service's demand is the
// program's own read.
const Store = Context.Service('test/SchemaConcurrencyStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readOne: (id) => `read ${id}`, writeOne: () => 'write' }),
);

const Item = Schema.Struct({
  id: Schema.Number,
  tags: Schema.Array(Schema.String),
  kind: Schema.Union([Schema.Literal('a'), Schema.Struct({ b: Schema.String })]),
});

const program = Effect.gen(function* () {
  const item = yield* Schema.decodeUnknownEffect(Item)(
    { id: 1, tags: ['x', 'y'], kind: { b: 'z' } },
    { concurrency: 'unbounded' },
  );
  const store = yield* Store;
  return `${store.readOne(item.id)} ${item.tags.join('')} ${item.kind.b}`;
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'read 1 xy z');
