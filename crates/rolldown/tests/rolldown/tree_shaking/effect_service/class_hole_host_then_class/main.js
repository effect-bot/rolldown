import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A callback that registers the service on its argument is first handed to
// the host (queueMicrotask, which calls it with no argument), then to a class
// constructor, which calls it with the instance. The class's register method
// receives the service, so Store keeps every method. Other still prunes.
const Store = Context.Service('test/ClassHoleHostThenClassStore');
const Other = Context.Service('test/ClassHoleHostThenClassOther');
class Registry {
  constructor(callback) {
    this.result = callback(this);
  }
  register(service) {
    return service.hostThenClassUsed();
  }
}

const program = Effect.gen(function* () {
  const store = yield* Store;
  const callback = (registry) => registry?.register(store);
  queueMicrotask(callback);
  const other = yield* Other;
  return [new Registry(callback).result, other.hostThenClassOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(
    Store,
    Store.of({ hostThenClassUsed: () => 'used', hostThenClassUnread: () => 'unread' }),
  ),
  Layer.succeed(
    Other,
    Other.of({ hostThenClassOtherUsed: () => 'other', hostThenClassOtherUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 'other']);
