import * as Effect from 'effect/Effect';
import { Store, relay } from './lib.js';

// Declared in the opposite order to the one main.js reaches them in.
export const second = () => relay(() => readLater('lateD'));

export const first = () => relay(() => readLater('lateC'));

const readLater = (name) =>
  Effect.gen(function* () {
    const store = yield* Store;
    return name === 'lateC' ? store.lateC() : store.lateD();
  });
