import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: run A hands its service object to module state, and run B, under
// a different Layer, reads a method of that object chosen at run time. Run B's
// Context never holds A's service, but the object is the same allocation, so
// every method of A's service stays. B's own unread method can still go.
const StoreA = Context.Service("test/RealSharedAllocationA")
const StoreB = Context.Service("test/RealSharedAllocationB")
const leakedUnusedHelper = () => "a2"
const LayerA = Layer.effect(
  StoreA,
  Effect.gen(function* () {
    const readA = () => "a1"
    const leakedUnused = () => leakedUnusedHelper()
    return StoreA.of({ readA, leakedUnused })
  })
)
const allocationBUnusedHelper = () => "b2"
const LayerB = Layer.effect(
  StoreB,
  Effect.gen(function* () {
    const readB = () => "b1"
    const allocationBUnused = () => allocationBUnusedHelper()
    return StoreB.of({ readB, allocationBUnused })
  })
)

const state = { leaked: undefined }
const runA = Effect.gen(function* () {
  const store = yield* StoreA
  state.leaked = store
  return store.readA()
})
const name = ["leaked", "Unused"].join("")
const runB = Effect.gen(function* () {
  return [(yield* StoreB).readB(), state.leaked[name]()]
})

assert.strictEqual(await Effect.runPromise(Effect.provide(runA, LayerA)), "a1")
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(runB, LayerB)), ["b1", "a2"])
