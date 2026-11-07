import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as ns from './c.js';
import * as ns2 from './d.js';

// A class method reads one service through a re-export chain (`export * from`
// over `export * as sub`). Only that export reaches class code: Store keeps
// every method, while Other, exported next to it, still prunes. A second class
// reads a namespace with a computed name, which can be any of its exports:
// Third keeps every method.
const Store = Context.Service('test/ClassHoleReexportChainStore');
const Other = Context.Service('test/ClassHoleReexportChainOther');
const Third = Context.Service('test/ClassHoleReexportChainThird');
class Chained {
  read() {
    return ns.sub.storeImpl.chainUsed();
  }
}
class Dynamic {
  read(name) {
    return ns2[name].dynamicFirst();
  }
}

const program = Effect.gen(function* () {
  yield* Store;
  yield* Third;
  const other = yield* Other;
  return [new Chained().read(), new Dynamic().read('thirdImpl'), other.chainOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(Store, ns.sub.storeImpl),
  Layer.succeed(Other, ns.sub.otherImpl),
  Layer.succeed(Third, ns2.thirdImpl),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'used',
  'first',
  'other',
]);
