import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: a class-expression key that redeclares the inherited `of`, so
// `Store.of(value)` is not the identity. Every method stays.
const Store = class extends Context.Service()("test/RealClassExpressionOverride") {
  static of(value) {
    return { readOne: () => value.overrideUnused() }
  }
}

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const overrideUnusedHelper = () => "overrideUnused"
    return Store.of({ readOne: () => "readOne", overrideUnused: () => overrideUnusedHelper() })
  })
)

const program = Effect.gen(function* () {
  return (yield* Store).readOne()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), "overrideUnused")
