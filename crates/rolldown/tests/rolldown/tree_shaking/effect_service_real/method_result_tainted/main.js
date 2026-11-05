import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Fiber from "effect/Fiber"

// A provider method returns the current Fiber, so its result holds a Context.
// Every method of the service must be kept.
const Store = Context.Service("test/RealMethodResultTainted")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

// Names every function, so serialization shows which methods a service has.
const replacer = (_key, value) => (typeof value === "function" ? "fn" : value)

const Probe = Context.Service("test/RealMethodResultTaintedProbe")
const ProbeLayer = Layer.succeed(Probe, { current: () => Fiber.getCurrent() })

const observe = (value) => (JSON.stringify(value, replacer).includes('"unused"') ? "unused" : "absent")

const program = Effect.gen(function* () {
  const store = yield* Store
  const probe = yield* Probe
  return [store.readOne(), observe(probe.current().context)]
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, ProbeLayer))),
  ["read", "unused"]
)
