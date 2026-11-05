import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Keyed lookups other than `yield*` by tracked keys, in both dual forms. They
// hand each service to code the pass does not follow, so `Store` and `Config`
// keep every method. `Other` is only read through `yield*`, so its unread
// method can still go.
const Store = Context.Service("test/RealLookupTrackedStore")
const Config = Context.Service("test/RealLookupTrackedConfig")
const Other = Context.Service("test/RealLookupTrackedOther")

const storeUnusedHelper = () => "storeUnused"
const configUnusedHelper = () => "configUnused"
const otherUnusedHelper = () => "otherUnused"

const layer = Layer.mergeAll(
  Layer.succeed(Store, { readOne: () => "read", storeUnused: () => storeUnusedHelper() }),
  Layer.succeed(Config, { name: () => "config", configUnused: () => configUnusedHelper() }),
  Layer.succeed(Other, { readOther: () => "other", otherUnused: () => otherUnusedHelper() })
)

const observed = []
const readStore = Effect.gen(function* () {
  return (yield* Store).readOne()
})

const program = Effect.gen(function* () {
  const other = yield* Other
  const first = yield* Effect.updateService(readStore, Store, (store) => {
    observed.push(store.storeUnused())
    return store
  })
  const second = yield* readStore.pipe(Effect.updateService(Store, (store) => {
    observed.push(store.storeUnused())
    return store
  }))
  const context = yield* Effect.context()
  const config = Context.getOrUndefined(context, Config)
  const sameConfig = Context.getOrUndefined(Config)(context)
  return [other.readOther(), first, second, observed, config.configUnused(), sameConfig === config]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  "other",
  "read",
  "read",
  ["storeUnused", "storeUnused"],
  "configUnused",
  true
])
