import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { readWith } from "./ext/reader.js"

// The service goes to an external module that is on disk. Its code reads only
// `readOne`, so the unread method can go.
const Store = Context.Service("test/RealExternalAnalysed")
const externalUnusedHelper = () => "unused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "one"
    const externalUnused = () => externalUnusedHelper()
    return Store.of({ readOne, externalUnused })
  })
)

const program = Effect.gen(function* () {
  return readWith(yield* Store)
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), "one")
