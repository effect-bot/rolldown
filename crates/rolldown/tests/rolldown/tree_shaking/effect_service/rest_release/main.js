import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The release callback reaches the acquired object through a rest parameter.
const K = Context.Service("test/Rest")
const log = []

const layer = Layer.effect(
  K,
  Effect.acquireRelease(
    Effect.succeed({ read: () => 1, unused: () => 2 }),
    (...args) => Effect.sync(() => log.push(args[0].unused()))
  )
)

const program = Effect.gen(function* () {
  return (yield* K).read()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 1)
assert.deepStrictEqual(log, [2])
