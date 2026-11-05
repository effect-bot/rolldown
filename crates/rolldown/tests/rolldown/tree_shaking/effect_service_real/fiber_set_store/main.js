import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as FiberSet from "effect/FiberSet"

// `FiberSet.run` stores the Fibers it starts in the set, and iterating the set
// yields them. Every method of the service must be kept.
const Store = Context.Service("test/RealFiberSetStore")

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

const program = Effect.scoped(
  Effect.gen(function* () {
    const store = yield* Store
    const set = yield* FiberSet.make()
    yield* FiberSet.run(set, Effect.never)
    const [fiber] = set
    return [store.readOne(), observe(fiber.context)]
  })
)

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
