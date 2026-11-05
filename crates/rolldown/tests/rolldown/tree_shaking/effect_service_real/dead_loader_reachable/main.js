import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: the same code loader runs, so the pass stays off and every method stays.
const Store = Context.Service("test/RealDeadLoaderReachable")
const liveUnusedHelper = () => "liveUnused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const liveUnused = () => liveUnusedHelper()
    return Store.of({ readOne, liveUnused })
  })
)

const called = () => new Function("return 1")()

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), called()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", 1])
