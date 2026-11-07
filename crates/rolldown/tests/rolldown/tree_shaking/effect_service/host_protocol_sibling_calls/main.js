import assert from 'node:assert';
import { inspect } from 'node:util';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Host formatting calls a service's protocol method, which calls a sibling
// method through `this`. Each case has its own service and helper, so each
// one fails on its own if the protocol method is kept but not analysed:
// String(x) -> toString, JSON.stringify -> toJSON, console.log and
// util.inspect -> [inspect.custom]. The unread method of each service goes.
const Stringed = Context.Service('test/HostProtocolString');
const Jsoned = Context.Service('test/HostProtocolJson');
const Logged = Context.Service('test/HostProtocolLog');
const Inspected = Context.Service('test/HostProtocolInspect');

const logCalls = [];
const layer = Layer.mergeAll(
  Layer.succeed(Stringed, {
    toString() {
      return 'S:' + this.stringHelper();
    },
    stringHelper: () => 's',
    stringUnused: () => 'unused',
  }),
  Layer.succeed(Jsoned, {
    toJSON() {
      return { j: this.jsonHelper() };
    },
    jsonHelper: () => 'j',
    jsonUnused: () => 'unused',
  }),
  Layer.succeed(Logged, {
    [inspect.custom]() {
      logCalls.push(this.logHelper());
      return 'Logged';
    },
    logHelper: () => 'l',
    logUnused: () => 'unused',
  }),
  Layer.succeed(Inspected, {
    [inspect.custom]() {
      return 'I:' + this.inspectHelper();
    },
    inspectHelper: () => 'i',
    inspectUnused: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const stringed = yield* Stringed;
  const jsoned = yield* Jsoned;
  const logged = yield* Logged;
  const inspected = yield* Inspected;
  console.log(logged);
  return [String(stringed), JSON.stringify(jsoned), logCalls, inspect(inspected)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'S:s',
  '{"j":"j"}',
  ['l'],
  'I:i',
]);
