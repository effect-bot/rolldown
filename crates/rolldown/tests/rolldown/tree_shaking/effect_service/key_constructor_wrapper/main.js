import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Keys created through a wrapper function and through an alias of
// `Context.Service`, each called with a constant string.
const makeKey = (id) => Context.Service(id);
const WrapperStore = makeKey('test/RealWrapperKey');
const Service = Context.Service;
const AliasStore = Service('test/RealAliasKey');
const wrapperUnusedHelper = () => 'wrapperUnused';
const WrapperLayer = Layer.effect(
  WrapperStore,
  Effect.gen(function* () {
    const readWrapper = () => 'readWrapper';
    const wrapperUnused = () => wrapperUnusedHelper();
    return WrapperStore.of({ readWrapper, wrapperUnused });
  }),
);
const aliasUnusedHelper = () => 'aliasUnused';
const AliasLayer = Layer.effect(
  AliasStore,
  Effect.gen(function* () {
    const readAlias = () => 'readAlias';
    const aliasUnused = () => aliasUnusedHelper();
    return AliasStore.of({ readAlias, aliasUnused });
  }),
);

const program = Effect.gen(function* () {
  return [(yield* WrapperStore).readWrapper(), (yield* AliasStore).readAlias()];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(WrapperLayer, AliasLayer))),
  ['readWrapper', 'readAlias'],
);
