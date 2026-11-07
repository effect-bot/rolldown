import assert from 'node:assert';
import { inspect } from 'node:util';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Logger from 'effect/Logger';
const Store = Context.Service('test/InspectCustomRecurse');
const printed = [];
const store = {
  inspectUsed: () => 'nested',
  inspectUnused: () => 'unused',
  [inspect.custom](_depth, _options, recurse) {
    const self = this;
    return recurse({
      [inspect.custom]() {
        return self.inspectUsed();
      },
    });
  },
};
const logger = Logger.make((options) => {
  printed.push(inspect(options.message));
});
const program = Effect.gen(function* () {
  const service = yield* Store;
  yield* Effect.log(service);
});
await Effect.runPromise(
  program.pipe(
    Effect.provideService(Store, store),
    Effect.provideService(Logger.CurrentLoggers, new Set([logger])),
  ),
);
assert.deepStrictEqual(printed, ['[ nested ]']);
