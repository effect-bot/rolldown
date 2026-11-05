import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Fiber from "effect/Fiber"

// As `completion_replacement_helper`, but the helper escapes into an array, so
// its parameter is unresolved. Every method of the service must be kept.
const Store = Context.Service("test/RealCompletionReplacementEscapedHelper")

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

function install(target, effect) {
  target._exit = effect
}
const installers = [install]

const program = Effect.gen(function* () {
  const store = yield* Store
  const fiber = yield* Effect.forkChild(
    Effect.gen(function* () {
      return (yield* Store).readOne()
    })
  )
  assert.strictEqual(yield* Fiber.join(fiber), "read")
  installers[0](fiber, Effect.gen(function* () {
    const inner = yield* Effect.forkChild(Effect.sync(() => 0))
    return inner.context
  }))
  return [store.readOne(), observe(yield* Fiber.join(fiber))]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
