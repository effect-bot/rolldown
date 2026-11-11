import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { reachedLate } from './late.js';

export const Store = Context.Service('test/DeferredDormantEagerFacts');

// Declared before the provider: removing `factsBefore` reads this binding, which
// must be initialised.
const beforeImpl = () => 'before';

export const layer = Layer.succeed(
  Store,
  Store.of({
    factsUsed: () => reachedLate,
    factsLate: () =>
      Effect.gen(function* () {
        return (yield* Store).factsAfter() + ' read';
      }),
    factsAfter: () => afterImpl(),
    factsBefore: beforeImpl,
    factsUnread: () => 'unread',
  }),
);

// Declared after the provider, reached through a call once the module ran.
const afterImpl = () => 'late';

// Nothing reaches these: an object, a class with methods and a function.
export const dormantObject = { dormantMethod: () => 1, other: [1, 2] };
export class DormantClass {
  dormantClassMethod() {
    return 2;
  }
}
export function dormantFunction() {
  return new DormantClass().dormantClassMethod();
}
