import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The base case, but the entry module exports a function. Whoever imports the
// bundle can call it with values whose getters run inside the program, so
// nothing may be removed.
const Store = Context.Service("test/Store")

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
  const store = yield* Store
  return store.readOne()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), "read")

export const run = (input) => Effect.runPromise(Effect.provide(Effect.map(program, (value) => `${value}${input}`), StoreLayer))
