import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Members outside the public types of a MemoMap, Context or Layer are empty
// slots (Tim's decision): reading Store through a MemoMap's `map` or a
// Context's `base`, or writing it to a Layer and reading it back, keeps
// nothing. Only `readOne` is read through a public path.
const Store = Context.Service('test/ContainerPrivateMember');
const StoreLayer = Layer.effect(
  Store,
  Effect.sync(() =>
    Store.of({
      readOne: () => 'r',
      privateMemo: () => 'm',
      privateBase: () => 'b',
      privateWrite: () => 'w',
    }),
  ),
);

// Probe is built with StoreLayer, so the memo map it is built with holds
// StoreLayer's Context.
const Probe = Context.Service('test/ContainerPrivateMemberProbe');
const ProbeLayer = Layer.effect(
  Probe,
  Effect.gen(function* () {
    const store = yield* Store;
    const memoMap = yield* Layer.CurrentMemoMap;
    const fromMemo = [];
    for (const entry of memoMap.map.values()) {
      const built = entry.deferred.effect?.value;
      if (built !== undefined) fromMemo.push(Context.get(built, Store).privateMemo());
    }
    const context = yield* Effect.context();
    const fromBase = context.base.get(Store.key).privateBase();
    StoreLayer.extra = store;
    const written = StoreLayer.extra.privateWrite();
    return Probe.of({ out: [store.readOne(), fromMemo, fromBase, written] });
  }),
);

const program = Effect.gen(function* () {
  return (yield* Probe).out;
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.provide(ProbeLayer, StoreLayer))),
  ['r', ['m'], 'b', 'w'],
);
