import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A container built from literals is enumerated after a write through an alias stored a
// Context in it. Every method of the service must be kept.
const Store = Context.Service("test/RealAliasMutation")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const observe = (context) => {
  const box = { mode: "fast" }
  const alias = box
  alias.held = context
  for (const value of Object.values(box)) {
    if (typeof value !== "object") continue
    const map = Object.values(value).find((inner) => inner instanceof Map)
    if (map) return map.get("test/RealAliasMutation").unused()
  }
}

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
