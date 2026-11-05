import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { readWith } from './ext/reader.js';

// Store goes to an external module. Its code is on disk but not scanned yet, so
// nothing is known about what it reads: Store keeps every method. Other is
// never handed over and loses its unread method.
const Store = Context.Service('test/RealExternalAnalysedStore');
const Other = Context.Service('test/RealExternalAnalysedOther');
const externalUnusedHelper = () => 'unused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'one';
    const externalUnused = () => externalUnusedHelper();
    return Store.of({ readOne, externalUnused });
  }),
);
const externalOtherUnusedHelper = () => 'otherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'other';
    const externalOtherUnused = () => externalOtherUnusedHelper();
    return Other.of({ readOther, externalOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  return [readWith(yield* Store), (yield* Other).readOther()];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['one', 'other'],
);
