import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Barrel from './barrel.js';
import { runHooks } from './hooks.js';
import * as Named from './named.js';
import './plugin.js';
import { viaTable } from './transitive.js';
import * as Whole from './wholesale.js';

// The readers live in top-level statements of other modules. Most of those
// statements have no side effects, so an analysis may run them only once
// reachable code references what they declare. Each path reads its own method:
// a statement the analysis never runs loses that method and the bundle throws.
// Only `lazyUnused` is read nowhere.
const Store = Context.Service('test/LazyTopLevelStore');
const lazyUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    transitiveUsed: () => 'transitive',
    namespaceUsed: () => 'namespace',
    barrelUsed: () => 'barrel',
    wholesaleUsed: () => 'wholesale',
    initializerUsed: () => 'initializer',
    lazyUnused: () => lazyUnusedHelper(),
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const [pick] = Object.values(Whole);
  return [
    viaTable(store),
    Named.viaNamespace(store),
    Barrel.viaBarrel(store),
    pick(store),
    ...runHooks(store),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'transitive',
  'namespace',
  'barrel',
  'wholesale',
  'initializer',
]);
