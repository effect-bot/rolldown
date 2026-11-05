import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Fiber from "effect/Fiber"

// Row 15 keeps each Fiber's origin, and joining an element read by a numeric index
// joins every origin. One origin succeeds with a forked Fiber's Context. Every
// operand matches a row. Every method of the service must be kept.
const Store = Context.Service("test/RealAllMixedOriginJoin")

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
const observe = (value) => (JSON.stringify(value, replacer).includes('"unused"') ? "unused" : "absent")

const leak = Effect.gen(function* () {
  const fiber = yield* Effect.forkChild(Effect.sync(() => 0))
  return fiber.context
})

const program = Effect.gen(function* () {
  const store = yield* Store
  const fibers = yield* Effect.all([Effect.forkChild(Effect.sync(() => "a")), Effect.forkChild(leak)])
  const observations = []
  for (let i = 0; i < fibers.length; i++) {
    observations.push(observe(yield* Fiber.join(fibers[i])))
  }
  return [store.readOne(), observations.join(",")]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "absent,unused"])
