// Built by `missing_declarations` and `unreadable_declarations` (mod.rs) in a
// temporary directory, against a copy of `effect` without usable `.d.ts`
// files. The pass can't model `effect`, so both methods must stay.
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/DeclarationsStore');

const program = Effect.gen(function* () {
  const store = yield* Store;
  return store.declarationsUsed();
});

const layer = Layer.succeed(
  Store,
  Store.of({ declarationsUsed: () => 'used', declarationsUnused: () => 'unused' }),
);
console.log(await Effect.runPromise(Effect.provide(program, layer)));
