import assert from 'node:assert';
import * as ConfigProvider from 'effect/ConfigProvider';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';
// A public type that cannot hold a service keeps the service out of the API. One
// variable holds a service and then a record of strings, which is passed where
// ConfigProvider.fromEnvRecord takes a Record<string, string | undefined>. Another
// holds a service and then a boolean, which a predicate hands back through
// Option.exists, declared to return a boolean; reading the keys of that result
// must not read the service. Each service's unread method is removed.
const Param = Context.Service('test/BoundRejectParam');
const Return = Context.Service('test/BoundRejectReturn');
const layer = Layer.mergeAll(
  Layer.succeed(Param)({ paramUsed: () => 'param', paramUnused: () => 'unused' }),
  Layer.succeed(Return)({ returnUsed: () => 'return', returnUnused: () => 'unused' }),
);
const program = Effect.gen(function* () {
  let value = yield* Param;
  const p = value.paramUsed();
  value = { METHOD: 'GET' };
  const node = yield* ConfigProvider.fromEnvRecord(value).load(['METHOD']);
  let result = yield* Return;
  const r = result.returnUsed();
  result = true;
  const exists = Option.exists(Option.some(1), () => result);
  return [p, r, node?._tag === 'Value' ? node.value : 'none', exists, Object.keys(exists).length];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'param',
  'return',
  'GET',
  true,
  0,
]);
