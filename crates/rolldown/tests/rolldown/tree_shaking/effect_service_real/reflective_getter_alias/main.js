import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Review finding: an aliased Object.getOwnPropertyDescriptor reads the `mapUnsafe`
// getter of the Context prototype. Every method of the service must be kept.
const Store = Context.Service("test/RealGetterAlias")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const descriptor = Object.getOwnPropertyDescriptor

const observe = (context) =>
  descriptor(Object.getPrototypeOf(context), "mapUnsafe")
    .get.call(context)
    .get("test/RealGetterAlias")
    .unused()

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
