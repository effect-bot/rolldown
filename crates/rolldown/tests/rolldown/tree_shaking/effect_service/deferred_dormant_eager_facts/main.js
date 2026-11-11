import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import { Store, layer } from './lib.js';

const program = Effect.gen(function* () {
  const store = yield* Store;
  const late = store.factsUsed();
  return [late(), yield* store.factsLate()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'late',
  'late read',
]);
