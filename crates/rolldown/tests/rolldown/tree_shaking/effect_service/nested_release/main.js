import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Two nested acquisitions of the same object, each with a release that reads
// a different method. Both readers must count.
const K = Context.Service("test/Nested")
const log = []

const layer = Layer.effect(
  K,
  Effect.acquireRelease(
    Effect.acquireRelease(
      Effect.succeed({ read: () => 1, inner: () => 2, outer: () => 3 }),
      (s) => Effect.sync(() => log.push(s.inner()))
    ),
    (s) => Effect.sync(() => log.push(s.outer()))
  )
)

const program = Effect.gen(function* () {
  return (yield* K).read()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 1)
assert.deepStrictEqual(log, [3, 2])
