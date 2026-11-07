import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { pipe } from 'effect/Function';
import * as Layer from 'effect/Layer';

// Effect's own functions called through `.call`, `.apply` and `.bind`: each
// passes the service to a function that reads one of its methods, which
// stays; the method nothing reads is removed.
const Store = Context.Service('test/BuiltinCallApplyBind');
const builtinHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store, {
  viaPipeCall: () => 'c',
  viaMapApply: () => 'a',
  viaMapBind: () => 'b',
  builtinUnused: () => builtinHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  const piped = pipe.call(undefined, store, (s) => s.viaPipeCall());
  const applied = yield* Effect.map.apply(null, [Effect.succeed(store), (s) => s.viaMapApply()]);
  const bound = yield* Effect.map.bind(null, Effect.succeed(store))((s) => s.viaMapBind());
  return [piped, applied, bound];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'c',
  'a',
  'b',
]);
