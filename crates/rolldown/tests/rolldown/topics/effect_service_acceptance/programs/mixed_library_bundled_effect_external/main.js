import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Lib from '.\/lib\/index.js';

// The application provides its own service next to the library's and runs
// the library's effect in its runtime.
class Config extends Context.Service()('app/Config') {}

const configGetHelper = () => 'three words here';
const appUnusedHelper = () => 'unused';

const ConfigLive = Layer.succeed(Config, {
  get: () => configGetHelper(),
  appUnused: () => appUnusedHelper(),
});

const program = Effect.gen(function* () {
  const config = yield* Config;
  const words = yield* Lib.countWords(config.get());
  return { words };
});

const result = await Effect.runPromise(
  Effect.provide(program, Layer.mergeAll(ConfigLive, Lib.StoreLive)),
);
assert.deepStrictEqual(result, { words: 3 });
console.log(JSON.stringify(result));
