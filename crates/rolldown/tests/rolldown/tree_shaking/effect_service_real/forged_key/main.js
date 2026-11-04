import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A hand-made key object with the same key string looks the service up without the
// tracked key. Every method of the service must be kept.
const Store = Context.Service("test/RealForgedKey")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

import * as ContextApi from "effect/Context"

const observe = (context) => ContextApi.get(context, { key: "test/RealForgedKey" }).unused()

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
