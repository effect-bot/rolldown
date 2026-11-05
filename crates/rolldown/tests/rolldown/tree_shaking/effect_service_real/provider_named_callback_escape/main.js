import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Named provider callbacks hand themselves out through their inner names, so
// another caller gets an object from the same literal. One service per path
// that follows returns: `Effect.sync`, `Effect.gen` and an `Effect.fnUntraced`
// constructor. The Layer factory is declared before the constructor so the
// constructor's literal is a known provider object when it is visited.
const SyncStore = Context.Service("test/RealNamedCallbackSync")
const GenStore = Context.Service("test/RealNamedCallbackGen")
const MakeStore = Context.Service("test/RealNamedCallbackMake")

let exposedSync
const SyncLayer = Layer.effect(
  SyncStore,
  Effect.sync(function syncInner() {
    exposedSync = syncInner
    return { readOne: () => "read", syncUnused: () => "syncUnused" }
  })
)

let exposedGen
const GenLayer = Layer.effect(
  GenStore,
  Effect.gen(function* genInner() {
    exposedGen = genInner
    return { readOne: () => "read", genUnused: () => "genUnused" }
  })
)

function makeLayer() {
  return Layer.effect(MakeStore, make())
}

let exposedMake
const make = Effect.fnUntraced(function* makeInner() {
  exposedMake = makeInner
  const readOne = () => "read"
  const makeUnused = () => "makeUnused"
  return { readOne, makeUnused }
})

const program = Effect.gen(function* () {
  return [(yield* SyncStore).readOne(), (yield* GenStore).readOne(), (yield* MakeStore).readOne()]
})

const layer = Layer.mergeAll(SyncLayer, GenLayer, makeLayer())
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ["read", "read", "read"])
assert.strictEqual(exposedSync().syncUnused(), "syncUnused")
assert.strictEqual(exposedGen().next().value.genUnused(), "genUnused")
assert.strictEqual(exposedMake().next().value.makeUnused(), "makeUnused")
