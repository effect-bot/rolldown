import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: a named class expression whose body refers to its own inner name,
// which leaks the key like `this` would. Every method stays.
const Store = class Inner extends Context.Service()("test/RealClassExpressionInnerName") {
  static readUnused() {
    return Effect.map(Inner, (store) => store.innerNameUnused())
  }
}
const innerNameUnusedHelper = () => "innerNameUnused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const innerNameUnused = () => innerNameUnusedHelper()
    return Store.of({ readOne, innerNameUnused })
  })
)

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), yield* Store.readUnused()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", "innerNameUnused"])
