import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A user method named like a builtin returns a captured Context: a clean-looking
// receiver does not make `get` a builtin. Every method of the service must be kept.
const Store = Context.Service("test/RealCleanReceiverMethod")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const observe = (context) =>
  Object.values({ get: () => context }.get())
    .find((value) => value instanceof Map)
    .get("test/RealCleanReceiverMethod")
    .unused()

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
