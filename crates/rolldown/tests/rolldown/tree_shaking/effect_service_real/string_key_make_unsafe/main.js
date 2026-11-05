import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// `Context.makeUnsafe` builds a Context from a map keyed by strings.
const Store = Context.Service("test/RealMakeUnsafe")

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
  return [store.readOne(), typeof Context.makeUnsafe(new Map())]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "object"])
