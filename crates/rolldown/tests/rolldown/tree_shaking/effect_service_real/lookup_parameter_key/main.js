import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A keyed lookup whose key is a parameter, called only with a tracked key:
// that key keeps every method, other keys can still be pruned.
const Store = Context.Service("test/RealParamLookupStore")
const Other = Context.Service("test/RealParamLookupOther")
const lookupIn = (context, key) => Context.get(context, key)
const paramStoreUnusedHelper = () => "paramStoreUnused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const paramStoreUnused = () => paramStoreUnusedHelper()
    return Store.of({ readOne, paramStoreUnused })
  })
)
const paramOtherUnusedHelper = () => "paramOtherUnused"
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => "readOther"
    const paramOtherUnused = () => paramOtherUnusedHelper()
    return Other.of({ readOther, paramOtherUnused })
  })
)

const program = Effect.gen(function* () {
  const context = yield* Effect.context()
  return [(yield* Other).readOther(), lookupIn(context, Store).paramStoreUnused()]
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ["readOther", "paramStoreUnused"]
)
