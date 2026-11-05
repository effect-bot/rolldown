import assert from 'node:assert';
import { pathToFileURL } from 'node:url';
import * as Effect from 'effect/Effect';

// The consumer is not bundled. It imports the library bundle given on the
// command line and uses every exported key and Layer.
const Lib = await import(pathToFileURL(process.argv[2]).href);

const program = Effect.gen(function* () {
  const store = yield* Lib.Store;
  const counter = yield* Lib.Counter;
  const described = yield* Lib.describe;
  return [
    store.read('k'),
    store.write('k', 1),
    store.keys(),
    counter.increment(),
    counter.current(),
    counter.reset(),
    described,
  ];
});

const result = await Effect.runPromise(Effect.provide(program, Lib.AppLive));
assert.deepStrictEqual(result, [
  'store:k',
  'wrote k=1',
  ['a', 'b'],
  2,
  2,
  'reset',
  'store:x after 1 increments',
]);
console.log(JSON.stringify(result));
