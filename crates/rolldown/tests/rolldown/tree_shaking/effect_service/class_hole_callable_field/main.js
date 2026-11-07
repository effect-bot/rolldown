import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A function holds the service in one of its own properties and is handed to
// a class method, which reads the service off it. The function body never
// runs, but the service reaches class code through the function's field, so
// Store keeps every method. Other still prunes.
const Store = Context.Service('test/ClassHoleCallableFieldStore');
const Other = Context.Service('test/ClassHoleCallableFieldOther');
class User {
  use(callback) {
    return callback.service.callableFieldUsed();
  }
}

const program = Effect.gen(function* () {
  const callback = () => {};
  callback.service = yield* Store;
  const other = yield* Other;
  return [new User().use(callback), other.callableOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(
    Store,
    Store.of({ callableFieldUsed: () => 'used', callableFieldUnread: () => 'unread' }),
  ),
  Layer.succeed(
    Other,
    Other.of({ callableOtherUsed: () => 'other', callableOtherUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 'other']);
