import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Positive gate: Fibers indexed by a counted loop variable. A numeric index cannot
// name a Context internal, so the pass must still remove `writeOne`.
const Store = Context.Service("test/RealNumericIndex")

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

import * as Fiber from "effect/Fiber"
const program = Effect.gen(function* () {
  const fibers = yield* Effect.all([
    Effect.forkChild(Effect.gen(function* () { return (yield* Store).readOne() })),
    Effect.forkChild(Effect.succeed("two"))
  ])
  const results = []
  for (let i = 0; i < fibers.length; i++) {
    results.push(yield* Fiber.join(fibers[i]))
  }
  return results
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "two"])
