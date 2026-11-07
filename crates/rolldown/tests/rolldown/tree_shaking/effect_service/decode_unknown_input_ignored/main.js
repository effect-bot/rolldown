import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Predicate from 'effect/Predicate';
import * as Schema from 'effect/Schema';

// Services passed to Schema.decodeUnknownSync, whose input is typed `unknown`,
// and read back from the decoded value: a record holding the service under a
// Schema.Any field, and the service itself decoded by a struct whose field is
// declared as a function. The decoded result holds nothing from an ignored
// input (Tim's decision), so the method called on each decoded value may go.
const Store = Context.Service('test/DecodeIgnoredStore');
const Plugin = Context.Service('test/DecodeIgnoredPlugin');
const layer = Layer.mergeAll(
  Layer.succeed(Store)({ flush: () => 'flushed' }),
  Layer.succeed(Plugin)({ greet: () => 'hi' }),
);
const Holder = Schema.Struct({ store: Schema.Any });
const PluginShape = Schema.Struct({ greet: Schema.declare((u) => Predicate.isFunction(u)) });
const program = Effect.gen(function* () {
  const store = yield* Store;
  const plugin = yield* Plugin;
  return [
    Schema.decodeUnknownSync(Holder)({ store }).store.flush(),
    Schema.decodeUnknownSync(PluginShape)(plugin).greet(),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['flushed', 'hi']);
