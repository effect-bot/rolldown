import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A builtin callback over clean data returns clean data, which may be
// enumerated. The pass must still remove `writeOne`.
const Store = Context.Service("test/RealCleanBuiltinCallback")

const readHelper = () => "read"
const writeHelper = () => "write"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => readHelper()
    const writeOne = () => writeHelper()
    return Store.of({ readOne, writeOne })
  })
)

const doubled = [1, 2].map((x) => x * 2)

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), Object.keys(doubled).join(",")]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "0,1"])
