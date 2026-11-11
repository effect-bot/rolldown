import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { otherRead } from './other.js';

export const Store = Context.Service('test/DeferredActivationPaths');

// Reached through reexport.js's renamed re-export and directly from main.js.
export const readAliased = () =>
  Effect.gen(function* () {
    return (yield* Store).aliasRead();
  });

// Reached through a namespace member read.
export const readNamed = () =>
  Effect.gen(function* () {
    return (yield* Store).namedRead();
  });

// Names a statement declared further down, which names one in another module.
export const nestedLater = () => later();

// Reached only through the namespace member read with a computed name.
export const wholeHolder = {
  readWhole: () =>
    Effect.gen(function* () {
      return (yield* Store).wholeRead();
    }),
};

// Re-exported by the entry: the consumer may call it.
export const entryExported = () => 'entry';

const later = () => otherRead(Store);
