import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A record is not row 15, which takes a dense array literal only: it takes the
// fallback for an Effect call without a summary. Every method of the service must be kept.
const Store = Context.Service("test/RealAllRecordFallback")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const program = Effect.gen(function* () {
  const store = yield* Store
  const record = yield* Effect.all({ a: Effect.sync(() => 1) })
  return [store.readOne(), JSON.stringify(record)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", '{"a":1}'])
