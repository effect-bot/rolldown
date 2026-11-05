import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

import { inspect } from 'node:util';

// The service is passed to JSON.stringify, string conversion and
// util.inspect/console.log. Those read `toJSON`, `toString`/`valueOf`/
// `Symbol.toPrimitive` and `[util.inspect.custom]`, and nothing else. The
// data property and the getter are never removed. `formatterUnused` is.
const Store = Context.Service('test/FormatterProtocol');
const formatterUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    formatterVersion: 3,
    get formatterCount() {
      return 7;
    },
    formatterUsed: () => 'used',
    formatterUnused: () => formatterUnusedHelper(),
    toJSON() {
      return { label: 'store', version: this.formatterVersion };
    },
    toString() {
      return 'Store<store>';
    },
    [Symbol.for('nodejs.util.inspect.custom')]() {
      return 'Store(inspected)';
    },
    formatterLabel: 'store',
  }),
);

const logged = [];
const program = Effect.gen(function* () {
  const store = yield* Store;
  logged.push(inspect(store));
  console.log(store);
  return [
    store.formatterUsed(),
    JSON.stringify(store),
    `${store}`,
    store.formatterCount,
    store.formatterLabel,
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'used',
  '{"label":"store","version":3}',
  'Store<store>',
  7,
  'store',
]);
assert.deepStrictEqual(logged, ['Store(inspected)']);
