import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A keyed lookup whose key argument is not one declared key: either service may
// be the one looked up, so both keep every method.
const Store = Context.Service("test/RealLookupUnresolvedStore")
const Other = Context.Service("test/RealLookupUnresolvedOther")

const storeUnusedHelper = () => "storeUnused"
const otherUnusedHelper = () => "otherUnused"

const layer = Layer.mergeAll(
  Layer.succeed(Store, { readOne: () => "read", storeUnused: () => storeUnusedHelper() }),
  Layer.succeed(Other, { readOther: () => "other", otherUnused: () => otherUnusedHelper() })
)

const program = Effect.gen(function* () {
  const store = yield* Store
  const other = yield* Other
  const key = Number("1") === 1 ? Other : Store
  const context = yield* Effect.context()
  return [store.readOne(), other.readOther(), Context.getOrUndefined(context, key).otherUnused()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ["read", "other", "otherUnused"])
