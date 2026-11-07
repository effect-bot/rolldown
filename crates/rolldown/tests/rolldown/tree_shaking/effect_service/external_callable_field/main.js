import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { register } from './late.js';

// A function holds the service in one of its own properties, set by a helper
// when the module loads, and is handed to an external module later. The module
// never calls the function: it reads the property and calls a method of the
// service there. The service reaches code the pass can't see through the
// function's field, so Store keeps every method. Other is never handed over
// and still prunes. The field is written before the function escapes, so the
// escape itself must hand over the function's fields.
const Store = Context.Service('test/ExternalCallableFieldStore');
const Other = Context.Service('test/ExternalCallableFieldOther');
const impl = Store.of({ callableFieldFirst: () => 'first', callableFieldSecond: () => 'second' });

const withService = (service) => {
  const callback = () => {};
  callback.service = service;
  return callback;
};
const callback = withService(impl);

const program = Effect.gen(function* () {
  const other = yield* Other;
  const registered = yield* Effect.sync(() => register(callback));
  return [registered, other.callableOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(Store, impl),
  Layer.succeed(
    Other,
    Other.of({ callableOtherUsed: () => 'other', callableOtherUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'first',
  'other',
]);
