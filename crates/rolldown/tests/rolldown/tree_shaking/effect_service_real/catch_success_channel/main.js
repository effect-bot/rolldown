import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A recovery callback's Effect becomes the success, here the live Context.
// Every method of the service must be kept.
const Store = Context.Service("test/RealCatchSuccessChannel")

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
  const context = yield* Effect.fail(1).pipe(Effect.catch(() => Effect.context()))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
