import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';

export const Store = Context.Service('test/DeferredLateArrivals');

// The shared helper: every caller's callback flows through `make`.
export const relay = (make) => Effect.flatMap(Effect.void, () => make());

export const callerA = () =>
  relay(() =>
    Effect.gen(function* () {
      return (yield* Store).lateA();
    }),
  );

export const callerB = () =>
  relay(() =>
    Effect.gen(function* () {
      return (yield* Store).lateB();
    }),
  );
