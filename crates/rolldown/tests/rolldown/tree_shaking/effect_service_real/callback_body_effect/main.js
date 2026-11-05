import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Fiber from "effect/Fiber"

// A coercion hook stores the live Context and returns a number: its body counts
// although its result is clean. Every method of the service must be kept.
const Store = Context.Service("test/RealCallbackBodyEffect")

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

const box = {}
const hook = {
  valueOf() {
    box.ctx = Fiber.getCurrent().context
    return 0
  }
}
const observe = () => {
  Number(hook)
  return JSON.stringify(box, replacer).includes('"unused"') ? "unused" : "absent"
}

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
