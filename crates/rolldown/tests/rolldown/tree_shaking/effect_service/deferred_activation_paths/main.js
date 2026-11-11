import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as lib from './lib.js';
import { Store, nestedLater, readAliased as direct } from './lib.js';
import { viaAlias } from './reexport.js';

export { entryExported } from './lib.js';

// Every lazy statement in lib.js, other.js and reexport.js is deferred: nothing
// references it until the solve reaches code that does. Each path must still
// generate it, once, even when two paths reach the same statement.
const wholeName = ['whole', 'Holder'].join('');
const program = Effect.gen(function* () {
  return [
    yield* viaAlias(),
    yield* direct(),
    yield* lib.readNamed(),
    yield* nestedLater(),
    // A member read with a computed name: every export of the namespace.
    yield* lib[wholeName].readWhole(),
  ];
});

const layer = Layer.succeed(
  Store,
  Store.of({
    aliasRead: () => 'alias',
    namedRead: () => 'named',
    laterRead: () => 'later',
    wholeRead: () => 'whole',
    deferredPathsUnread: () => 'unread',
  }),
);

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'alias',
  'alias',
  'named',
  'later',
  'whole',
]);
