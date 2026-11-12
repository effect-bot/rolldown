import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { nameIt as nameLater } from './name-later.js';
import { nameIt as nameFirst } from './name-first.js';

// The pass does not analyse a class's body, so a static method's argument
// crosses into the class.
class Registry {
  static add(o) {
    return o;
  }
}

// The object crosses the class first and the module hole that names it second.
const later = {};
Registry.add(later);
nameLater(later);
const Later = Context.Service(later.name);
const LaterLayer = Layer.succeed(Later, {
  laterUsed: () => 'later',
  laterUnused: () => 'unused',
});

// Control: the module hole first, then the class.
const first = {};
nameFirst(first);
Registry.add(first);
const First = Context.Service(first.name);
const FirstLayer = Layer.succeed(First, {
  firstUsed: () => 'first',
  firstUnused: () => 'unused',
});

const program = Effect.gen(function* () {
  const a = yield* Later;
  const b = yield* First;
  return [a.laterUsed(), b.firstUsed()];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(LaterLayer, FirstLayer))),
  ['later', 'first'],
);
