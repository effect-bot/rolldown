import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Keys declared as class expressions bound to module-level bindings, as
// bundled libraries emit them (`var K = class extends Context.Service()(...) {}`),
// with a `var` and a `const` binding that are never reassigned.
var VarStore = class extends Context.Service()(`test/RealClassExpressionVar`) {}
const ConstStore = class extends Context.Service()("test/RealClassExpressionConst") {}
const varClassUnusedHelper = () => "varClassUnused"
const VarLayer = Layer.effect(
  VarStore,
  Effect.gen(function* () {
    const readVar = () => "readVar"
    const varClassUnused = () => varClassUnusedHelper()
    return VarStore.of({ readVar, varClassUnused })
  })
)
const constClassUnusedHelper = () => "constClassUnused"
const ConstLayer = Layer.effect(
  ConstStore,
  Effect.gen(function* () {
    const readConst = () => "readConst"
    const constClassUnused = () => constClassUnusedHelper()
    return ConstStore.of({ readConst, constClassUnused })
  })
)

const program = Effect.gen(function* () {
  return [(yield* VarStore).readVar(), (yield* ConstStore).readConst()]
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(VarLayer, ConstLayer))),
  ["readVar", "readConst"]
)
