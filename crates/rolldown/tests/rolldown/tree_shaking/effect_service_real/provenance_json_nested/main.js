import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// JSON.stringify recurses into a Context nested in user data, and Context.toJSON lists
// each service with its literal properties. Every property of the service must be kept.
const Store = Context.Service("test/RealJsonNested")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused, version: "v1" })
  })
)

const settings = { mode: "fast" }

const observe = (context) => {
  const json = JSON.stringify({ a: [settings, { context }] })
  return json.includes('"version":"v1"') ? "unused" : json
}

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
