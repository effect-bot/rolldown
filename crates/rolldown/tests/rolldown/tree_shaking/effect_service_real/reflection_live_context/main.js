import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The same reflective reads as reflection_clean_data, applied to a live Context: each
// one reaches the service without its key. Every method of the service must be kept.
const Store = Context.Service("test/RealLiveContext")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const isStore = (value) => value !== null && typeof value === "object" && typeof value.readOne === "function"

const probes = (context) => {
  // Enumeration: the service map is an own property value of the Context.
  const viaValues = Object.values(context)
    .find((value) => value instanceof Map)
    .get("test/RealLiveContext")
  // A member name the pass cannot evaluate.
  const name = ["map", "Unsafe"].join("")
  const viaName = context[name].get("test/RealLiveContext")
  // A JSON replacer sees every service through Context.toJSON.
  let viaJson
  JSON.stringify({ context }, (_key, value) => {
    if (isStore(value)) viaJson = value
    return value
  })
  return [viaValues, viaName, viaJson].map((service) => service.unused())
}

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), probes(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  "read",
  ["unused", "unused", "unused"]
])
