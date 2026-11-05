import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The one-argument `Effect.map(f)` is not row 14: it takes the fallback for an
// Effect call without a summary. Every method of the service must be kept.
const Store = Context.Service("test/RealMapCurriedFallback")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const load = Effect.sync(() => ({ mode: "fast" }))

const program = Effect.gen(function* () {
  const store = yield* Store
  const settings = yield* load.pipe(Effect.map((value) => value))
  return [store.readOne(), JSON.stringify(settings)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", '{"mode":"fast"}'])
