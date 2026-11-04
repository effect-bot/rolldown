import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Effect.tap succeeds with its input, not with its callback's result: the callback
// returns user data, but the value enumerated here is the live Context.
// Every method of the service must be kept.
const Store = Context.Service("test/RealTap")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const program = Effect.gen(function* () {
  const store = yield* Store
  const tapped = yield* Effect.tap(
    Effect.withFiber((fiber) => Effect.succeed(fiber.context)),
    () => Effect.succeed({ mode: "fast" })
  )
  const map = Object.values(tapped).find((value) => value instanceof Map)
  return [store.readOne(), map.get("test/RealTap").unused()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
