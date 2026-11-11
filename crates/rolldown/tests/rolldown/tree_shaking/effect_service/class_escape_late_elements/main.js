import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The class body is not analysed: it stores a handler on what it is given,
// and the handler calls a method the program never names.
const Store = Context.Service('test/ClassEscapeLateElements');
class Installer {
  constructor(target) {
    target.handler = (store) => store.lateUnread();
  }
}

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.lateUsed(), handle(store)];
});

// `box.handler` is read after `box` is handed to the class, so it is the
// class's handler. The pass applies the read first.
const box = {};
new Installer(box);
const handle = box.handler;

const layer = Layer.succeed(
  Store,
  Store.of({ lateUsed: () => 'used', lateUnread: () => 'unread' }),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 'unread']);
