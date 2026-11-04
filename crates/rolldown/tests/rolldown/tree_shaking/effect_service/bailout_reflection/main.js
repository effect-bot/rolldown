import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The base case, but the program reaches the global object, where Effect
// publishes the running Fiber, so nothing may be removed.
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

assert.strictEqual(typeof Reflect.get(globalThis, "Math"), "object")
assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), "read")
