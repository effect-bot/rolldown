import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Two runs with different Layers. Run A reads its service by a name built at
// run time, so any method of A's service may be used. Run B reads statically
// and never shares a service with run A, so B's unread method can go.
const StoreA = Context.Service("test/RealScopedRootA")
const StoreB = Context.Service("test/RealScopedRootB")
const rootAUnusedHelper = () => "a2"
const LayerA = Layer.effect(
  StoreA,
  Effect.gen(function* () {
    const readA = () => "a1"
    const rootAUnused = () => rootAUnusedHelper()
    return StoreA.of({ readA, rootAUnused })
  })
)
const rootBUnusedHelper = () => "b2"
const LayerB = Layer.effect(
  StoreB,
  Effect.gen(function* () {
    const readB = () => "b1"
    const rootBUnused = () => rootBUnusedHelper()
    return StoreB.of({ readB, rootBUnused })
  })
)

const name = ["rootA", "Unused"].join("")
const runA = Effect.gen(function* () {
  const store = yield* StoreA
  return [store.readA(), store[name]()]
})
const runB = Effect.gen(function* () {
  return (yield* StoreB).readB()
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(runA, LayerA)), ["a1", "a2"])
assert.strictEqual(await Effect.runPromise(Effect.provide(runB, LayerB)), "b1")
