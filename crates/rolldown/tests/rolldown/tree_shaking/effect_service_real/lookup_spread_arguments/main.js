import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A spread moves the key argument of a keyed lookup: at run time the second
// argument is a hand-made key for `Store`, and the trailing `Other` is ignored.
const Store = Context.Service("test/RealLookupSpreadStore")
const Other = Context.Service("test/RealLookupSpreadOther")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    return Store.of({ readOne: () => "read", unused: () => "unused" })
  })
)
const OtherLayer = Layer.succeed(Other, { ok: () => "ok" })

const program = Effect.gen(function* () {
  const context = yield* Effect.context()
  return Context.get(...[context, { key: "test/RealLookupSpreadStore" }], Other).unused()
})

assert.strictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.merge(StoreLayer, OtherLayer))),
  "unused"
)
