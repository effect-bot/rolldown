import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: the lookup's key argument is chosen at run time, so either service
// may be read. Every method of both stays.
const Store = Context.Service("test/RealParamUnknownStore")
const Other = Context.Service("test/RealParamUnknownOther")
const lookupIn = (context, key) => Context.get(context, key)
const unknownStoreUnusedHelper = () => "unknownStoreUnused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const unknownStoreUnused = () => unknownStoreUnusedHelper()
    return Store.of({ readOne, unknownStoreUnused })
  })
)
const unknownOtherUnusedHelper = () => "unknownOtherUnused"
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => "readOther"
    const unknownOtherUnused = () => unknownOtherUnusedHelper()
    return Other.of({ readOther, unknownOtherUnused })
  })
)

const program = Effect.gen(function* () {
  const context = yield* Effect.context()
  const key = Number("1") === 1 ? Other : Store
  return [(yield* Store).readOne(), lookupIn(context, key).unknownOtherUnused()]
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ["readOne", "unknownOtherUnused"]
)
