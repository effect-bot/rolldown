import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// `Context.getOrUndefinedUnsafe` looks a service up by its key string.
const Store = Context.Service("test/RealGetOrUndefinedUnsafe")

const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const program = Effect.gen(function* () {
  const store = yield* Store
  const context = yield* Effect.context()
  return [store.readOne(), Context.getOrUndefinedUnsafe(context, "test/RealGetOrUndefinedUnsafe").unused()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
