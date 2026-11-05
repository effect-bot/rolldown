import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as ManagedRuntime from "effect/ManagedRuntime"

// Control: two runs on one ManagedRuntime share the service object it built
// once (the build counter checks that). Run A reads one method statically; run B reads a method chosen at run
// time on the same object, so every method of the shared service stays.
// A service provided only to run A still loses its unread method.
const Shared = Context.Service("test/RealSharedRuntime")
const Local = Context.Service("test/RealSharedRuntimeLocal")
const sharedRuntimeUnusedHelper = () => "s2"
let builds = 0
const SharedLayer = Layer.effect(
  Shared,
  Effect.gen(function* () {
    builds++
    const readShared = () => "s1"
    const sharedRuntimeUnused = () => sharedRuntimeUnusedHelper()
    return Shared.of({ readShared, sharedRuntimeUnused })
  })
)
const localRuntimeUnusedHelper = () => "l2"
const LocalLayer = Layer.effect(
  Local,
  Effect.gen(function* () {
    const readLocal = () => "l1"
    const localRuntimeUnused = () => localRuntimeUnusedHelper()
    return Local.of({ readLocal, localRuntimeUnused })
  })
)

const runtime = ManagedRuntime.make(SharedLayer)
const runA = Effect.gen(function* () {
  const shared = yield* Shared
  return [shared.readShared(), (yield* Local).readLocal()]
})
const name = ["sharedRuntime", "Unused"].join("")
const runB = Effect.gen(function* () {
  return (yield* Shared)[name]()
})

const resultA = await runtime.runPromise(Effect.provide(runA, LocalLayer))
const resultB = await runtime.runPromise(runB)
assert.deepStrictEqual([resultA, resultB, builds], [["s1", "l1"], "s2", 1])
await runtime.dispose()
