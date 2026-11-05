import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Computed names that the analysis resolves to finite sets of literals.
const Store = Context.Service('test/ComputedFinite');
const computedUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    computedA: () => 'a',
    computedB: () => 'b',
    computedC: () => 'c',
    computedD: () => 'd',
    computedEf: () => 'ef',
    computedUnused: () => computedUnusedHelper(),
  }),
);

const cond = process.env.EFFECT_FIXTURE_COND === 'b';
const program = Effect.gen(function* () {
  const svc = yield* Store;
  const out = [svc[cond ? 'computedB' : 'computedA']()];
  for (const n of ['computedC', 'computedD']) out.push(svc[n]());
  out.push(svc[['computed', 'Ef'].join('')]());
  return out;
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'a',
  'c',
  'd',
  'ef',
]);
