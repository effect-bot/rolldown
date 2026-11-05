import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Fiber from "effect/Fiber"

// A completed Fiber's Exit payload is replaced by the Context: the next join
// returns it. Every method of the service must be kept.
const Store = Context.Service("test/RealMutatedExitJoin")

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
  const fiber = yield* Effect.forkChild(
    Effect.gen(function* () {
      return (yield* Store).readOne()
    })
  )
  assert.strictEqual(yield* Fiber.join(fiber), "read")
  fiber._exit["~effect/Effect/args"] = fiber.context
  return [store.readOne(), observe(yield* Fiber.join(fiber))]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
