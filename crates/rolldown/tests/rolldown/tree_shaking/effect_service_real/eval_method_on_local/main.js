import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A method named `eval` on a local object is not the global `eval`.
const Store = Context.Service("test/RealEvalMethod")
const client = { eval: (script) => script.length }
const evalUnusedHelper = () => "evalUnused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const evalUnused = () => evalUnusedHelper()
    return Store.of({ readOne, evalUnused })
  })
)

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), client.eval("abc")]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", 3])
