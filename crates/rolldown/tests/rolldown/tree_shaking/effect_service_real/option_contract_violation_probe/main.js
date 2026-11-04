import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// CONTRACT-VIOLATING PROBE, not supported behaviour. effectServiceShakingAssumeNoReflection
// promises that user code never enumerates, indexes by a non-constant name or serializes
// an Effect runtime object. This program does all three on a live Context, and the
// assertions pin what the option then does: it trusts the promise, removes `unused`, and
// each probe finds it missing. In default mode, any one of the three keeps `unused`
// (see enumeration, dynamic_name and json_literal).
const Store = Context.Service("test/RealOptionProbe")

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
    .get("test/RealOptionProbe")
  // A member name the pass cannot evaluate.
  const name = ["map", "Unsafe"].join("")
  const viaName = context[name].get("test/RealOptionProbe")
  // A JSON replacer sees every service through Context.toJSON.
  let viaJson
  JSON.stringify({ context }, (_key, value) => {
    if (isStore(value)) viaJson = value
    return value
  })
  return [viaValues, viaName, viaJson].map((service) => typeof service.unused)
}

const program = Effect.gen(function* () {
  const store = yield* Store
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), probes(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  "read",
  ["undefined", "undefined", "undefined"]
])
