import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import {
  aliased,
  reassignedLet,
  reassignedVar,
  reexported,
  throughLet,
  viaImport,
} from 'effect-aliases';

// Immutable: each key is a literal reached only through consts, an import and
// a re-export.
const Fixed = Context.Service('test/ConstantKeyAliasesFixed');
// Mutable: each key started as a const literal, but a write changed it before
// the program reads it.
const ViaLet = Context.Service('test/ConstantKeyAliasesLet');
const ViaVar = Context.Service('test/ConstantKeyAliasesVar');
const ViaHop = Context.Service('test/ConstantKeyAliasesHop');
const layer = Layer.mergeAll(
  Layer.succeed(Fixed, {
    aliasedKey: () => 'aliased',
    importedKey: () => 'imported',
    reexportedKey: () => 'reexported',
    fixedUnused: () => 'unused',
  }),
  Layer.succeed(ViaLet, { letFirst: () => 'let first', letSecond: () => 'let second' }),
  Layer.succeed(ViaVar, { varFirst: () => 'var first', varSecond: () => 'var second' }),
  Layer.succeed(ViaHop, { hopFirst: () => 'hop first', hopSecond: () => 'hop second' }),
);

const program = Effect.gen(function* () {
  const fixed = yield* Fixed;
  const viaLet = yield* ViaLet;
  const viaVar = yield* ViaVar;
  const viaHop = yield* ViaHop;
  return [
    fixed[aliased](),
    fixed[viaImport](),
    fixed[reexported](),
    viaLet[reassignedLet](),
    viaVar[reassignedVar](),
    viaHop[throughLet](),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'aliased',
  'imported',
  'reexported',
  'let second',
  'var second',
  'hop second',
]);
