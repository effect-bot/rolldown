import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as H from './helpers.js';

// Provider and key in their own module, helpers reached through a namespace.
export const Store = Context.Service('test/RealCrossModule');

export const layer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => H.liveHelper();
    const writeOne = () => H.deadHelper();
    return Store.of({ readOne, writeOne });
  }),
);
