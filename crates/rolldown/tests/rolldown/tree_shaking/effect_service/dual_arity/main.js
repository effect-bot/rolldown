import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { pipe } from 'effect/Function';
import * as Layer from 'effect/Layer';
import { describeThree, describeTwo, runAll } from './duals.js';

// `Function.dual` with a numeric arity: a call with fewer arguments is data-last
// and returns a function waiting for the data. The services are only ever the
// captured `that`, never the enumerated `self`, so their unread methods can go.
const StoreTwo = Context.Service('test/DualArityTwo');
const StoreThree = Context.Service('test/DualArityThree');
// Predicate style: `runAll(store)` is a full call at run time even with one
// argument, and it calls every method of the store.
const StorePredicate = Context.Service('test/DualArityPredicate');

const AppLayer = Layer.mergeAll(
  Layer.succeed(StoreTwo, StoreTwo.of({ twoUsed: () => 'two', twoUnused: () => 'u2' })),
  Layer.succeed(StoreThree, StoreThree.of({ threeUsed: () => 'three', threeUnused: () => 'u3' })),
  Layer.succeed(
    StorePredicate,
    StorePredicate.of({ predicateFirst: () => 'p1', predicateSecond: () => 'p2' }),
  ),
);

const plain = { a: 'x', b: 'y' };

const program = Effect.gen(function* () {
  const two = yield* StoreTwo;
  const three = yield* StoreThree;
  const predicate = yield* StorePredicate;
  return [
    pipe(plain, describeTwo(two)),
    pipe(plain, describeThree(three, 'label')),
    runAll(predicate),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, AppLayer)), [
  ['x,y', 'two'],
  ['label', 'x,y', 'three'],
  ['p1', 'p2'],
]);
