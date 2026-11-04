import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Positive gate: JSON.stringify of nested user data that holds no Effect runtime
// object. The pass must still remove `writeOne`.
const Store = Context.Service("test/RealJsonClean")

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

const settings = { mode: "fast" }
const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), JSON.stringify({ a: [settings, { n: 1 }] })]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", '{"a":[{"mode":"fast"},{"n":1}]}'])
