import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The resolved `effect` package declares 4.0.0, not the version the operation
// summaries were checked against. The pass must keep every method and report it.
const Store = Context.Service("test/VersionMismatch")

const readHelper = () => "read"
const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => readHelper()
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const program = Effect.gen(function* () {
  return (yield* Store).readOne()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), "read")
