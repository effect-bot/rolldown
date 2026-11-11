import * as Effect from 'effect/Effect';

export const otherRead = (Store) =>
  Effect.gen(function* () {
    return (yield* Store).laterRead();
  });

// Nothing reaches this statement: it stays dormant under deferred generation.
export const neverReached = { dormantMethod: () => 'dormant' };
