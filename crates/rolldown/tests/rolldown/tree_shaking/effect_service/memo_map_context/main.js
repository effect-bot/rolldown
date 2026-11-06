import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';

const Store = Context.Service('test/RealMemoStore');
const memoUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const memoUnused = () => memoUnusedHelper();
    return Store.of({ readOne, memoUnused });
  }),
);
const name = ['memo', 'Unused'].join('');
// A key Effect's own modules declare: the memo map a Layer is built with
// holds the Contexts of the Layers built before it. `MemoMap.get` returns the
// memoised build of StoreLayer, and the program reads Store out of its Context.
const Probe = Context.Service('test/RealMemoMapProbe');
const ProbeLayer = Layer.effect(
  Probe,
  Effect.gen(function* () {
    const memoMap = yield* Layer.CurrentMemoMap;
    const built = memoMap.get(StoreLayer, yield* Scope.Scope);
    const out = built === undefined ? [] : [Context.get(yield* built, Store)[name]()];
    return Probe.of({ out });
  }),
);
const AppLayer = Layer.provide(ProbeLayer, StoreLayer);
const program = Effect.gen(function* () {
  return (yield* Probe).out;
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, AppLayer)), ['u']);
