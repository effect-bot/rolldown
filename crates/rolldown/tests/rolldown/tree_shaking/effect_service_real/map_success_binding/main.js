import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Row 14: `Effect.map(e, f)` succeeds with what `f` returns, here the forked
// Fiber's Context. Every operand matches a row, so only that transfer can keep
// `unused`. Every method of the service must be kept.
const Store = Context.Service("test/RealMapSuccessBinding")

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
  const context = yield* Effect.map(Effect.forkChild(Effect.sync(() => 0)), (fiber) => fiber.context)
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
