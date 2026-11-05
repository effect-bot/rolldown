import { alias } from './barrel.js';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `alias` resolves through the barrel back to `helper` in this module, which is
// still in its temporal dead zone when the provider is built, so building it
// throws. Removing the unread property would drop that exception.
const Store = Context.Service('test/RealProviderTdzImportCycle');

const program = Effect.gen(function* () {
  return (yield* Store).readOne();
});

export let caught = false;
try {
  const layer = Layer.succeed(Store, {
    readOne: () => 'read',
    tdzImportUnused: alias,
  });
  await Effect.runPromise(Effect.provide(program, layer));
} catch (error) {
  caught = error instanceof ReferenceError;
}

export const helper = () => 'unused';
