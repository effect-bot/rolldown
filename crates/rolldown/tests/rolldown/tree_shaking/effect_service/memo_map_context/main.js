import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';

// MemoMap is ignored (Tim's decision): it isn't meant for public use, so
// reaching Store through `Layer.CurrentMemoMap` and `MemoMap.get` and calling
// a method named at run time keeps nothing. Only `readOne` is read through a
// public path, and only that read is asserted.
const Store = Context.Service('test/RealMemoStore');
const memoUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.sync(() => Store.of({ readOne: () => 'r', memoUnused: () => memoUnusedHelper() })),
);
const name = ['memo', 'Unused'].join('');

const Probe = Context.Service('test/RealMemoMapProbe');
const ProbeLayer = Layer.effect(
  Probe,
  Effect.gen(function* () {
    const memoMap = yield* Layer.CurrentMemoMap;
    const built = memoMap.get(StoreLayer, yield* Scope.Scope);
    if (built !== undefined) Context.get(yield* built, Store)[name]?.();
    return Probe.of({});
  }),
);

const program = Effect.gen(function* () {
  yield* Probe;
  return (yield* Store).readOne();
});
assert.strictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.provideMerge(ProbeLayer, StoreLayer))),
  'r',
);
