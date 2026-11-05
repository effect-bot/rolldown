import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Controls for the temporal dead zone checks: each provider reads a binding
// that is initialised where the provider is built, so the unread property can
// go. A parameter read in the function body, a lexical loop-head binding read
// in the loop body, and a `var` loop-head binding (undefined, not throwing)
// read in the loop's right-hand side.
const ParameterBody = Context.Service("test/RealInitializedParameterBody")
const LoopBody = Context.Service("test/RealInitializedLoopBody")
const VarHead = Context.Service("test/RealInitializedVarHead")

const readParameterBody = Effect.gen(function* () {
  return (yield* ParameterBody).readOne()
})
const readLoopBody = Effect.gen(function* () {
  return (yield* LoopBody).readOne()
})
const readVarHead = Effect.gen(function* () {
  return (yield* VarHead).readOne()
})

function provideParameter(parameterHelper) {
  return Effect.runSync(
    Effect.provide(
      readParameterBody,
      Layer.succeed(ParameterBody, { readOne: () => "read", parameterUnused: parameterHelper })
    )
  )
}
assert.strictEqual(provideParameter(() => "unused"), "read")

const results = []
for (const bodyHelper of [() => "unused"]) {
  results.push(
    Effect.runSync(
      Effect.provide(readLoopBody, Layer.succeed(LoopBody, { readOne: () => "read", bodyUnused: bodyHelper }))
    )
  )
}
assert.deepStrictEqual(results, ["read"])

const heads = []
for (var varHelper of [
  Effect.runSync(Effect.provide(readVarHead, Layer.succeed(VarHead, { readOne: () => "read", varUnused: varHelper })))
]) {
  heads.push(varHelper)
}
assert.deepStrictEqual(heads, ["read"])
