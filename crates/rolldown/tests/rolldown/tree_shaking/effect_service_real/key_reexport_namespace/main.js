import assert from "node:assert"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import * as Lib from "./index.js"
import { BillingStore, Other } from "./keys.js"

// The barrel is used only for a constant, so the `Keys` namespace object is
// never read, but the key module counts as a namespace target. Its export
// specifier only names the binding.
assert.strictEqual(Lib.version, "1")

const reexportStoreUnusedHelper = () => "reexportStoreUnused"
const StoreLayer = Layer.effect(
  BillingStore,
  Effect.gen(function* () {
    const readStore = () => "readStore"
    const reexportStoreUnused = () => reexportStoreUnusedHelper()
    return BillingStore.of({ readStore, reexportStoreUnused })
  })
)
const reexportOtherUnusedHelper = () => "reexportOtherUnused"
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => "readOther"
    const reexportOtherUnused = () => reexportOtherUnusedHelper()
    return Other.of({ readOther, reexportOtherUnused })
  })
)

const program = Effect.gen(function* () {
  return [(yield* BillingStore).readStore(), (yield* Other).readOther()]
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ["readStore", "readOther"]
)
