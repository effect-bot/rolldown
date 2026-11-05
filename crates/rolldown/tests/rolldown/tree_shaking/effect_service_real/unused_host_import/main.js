import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { compileFunction } from "node:vm"

// Importing a Node module observes nothing: only calls do. The pass must still
// remove `writeOne`.
const Store = Context.Service("test/RealUnusedHostImport")

const readHelper = () => "read"
const writeHelper = () => "write"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => readHelper()
    const writeOne = () => writeHelper()
    return Store.of({ readOne, writeOne })
  })
)

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), typeof compileFunction]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "function"])
