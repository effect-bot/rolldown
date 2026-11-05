import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { BillingStore } from "./keys.js"

// Control: the importer of a re-exported key hands it to a function chosen at
// run time. The export does not vouch for its importers. Every method stays.
const sinks = [(key) => key]
const reexportOpaqueUnusedHelper = () => "reexportOpaqueUnused"
const StoreLayer = Layer.effect(
  BillingStore,
  Effect.gen(function* () {
    const readStore = () => "readStore"
    const reexportOpaqueUnused = () => reexportOpaqueUnusedHelper()
    return BillingStore.of({ readStore, reexportOpaqueUnused })
  })
)

const program = Effect.gen(function* () {
  const leaked = sinks[Number("0")](BillingStore)
  return [(yield* BillingStore).readStore(), leaked === BillingStore]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readStore", true])
