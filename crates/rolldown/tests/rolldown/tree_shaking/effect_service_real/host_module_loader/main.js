import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { getBuiltinModule } from "node:process"

// A host operation without a summary may run code in this realm, here through
// `process.getBuiltinModule`. Every method of the service must be kept.
const Store = Context.Service("test/RealHostModuleLoader")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const observe = (context) =>
  getBuiltinModule("vm").compileFunction('return ctx.mapUnsafe.get("test/RealHostModuleLoader").unused()', ["ctx"])(context)

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
