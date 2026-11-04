import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A user helper enumerates its parameter, and is called both with user data and with
// the live Context. Every method of the service must be kept.
const Store = Context.Service("test/RealTaintedParam")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const describe = (value) => Object.values(value)

const observe = (context) => {
  describe({ mode: "fast" })
  return describe(context)
    .find((value) => value instanceof Map)
    .get("test/RealTaintedParam")
    .unused()
}

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
