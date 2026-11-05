import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Fiber from "effect/Fiber"

// A completed Fiber's Exit is replaced by an Effect whose success is a Context:
// the next join runs it. Every method of the service must be kept.
const Store = Context.Service("test/RealCompletionReplacement")

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
  fiber._exit = Effect.gen(function* () {
    const inner = yield* Effect.forkChild(Effect.sync(() => 0))
    return inner.context
  })
  return [store.readOne(), observe(yield* Fiber.join(fiber))]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
