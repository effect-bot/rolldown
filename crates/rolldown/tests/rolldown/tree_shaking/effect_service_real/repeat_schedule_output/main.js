import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Fiber from "effect/Fiber"
import * as Schedule from "effect/Schedule"

// `Effect.repeat` succeeds with the schedule output, here the live Context.
// Every method of the service must be kept.
const Store = Context.Service("test/RealRepeatScheduleOutput")

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

const program = Effect.gen(function* () {
  const store = yield* Store
  const output = yield* Effect.repeat(
    Effect.succeed(7),
    Schedule.map(Schedule.recurs(0), () => Fiber.getCurrent().context)
  )
  return [store.readOne(), observe(output)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
