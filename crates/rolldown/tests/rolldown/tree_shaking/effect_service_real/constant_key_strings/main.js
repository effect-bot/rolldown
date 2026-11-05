import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Keys whose strings are constants but not plain string literals: a template
// without substitutions and a concatenation of literals.
const TemplateStore = Context.Service(`test/RealTemplateKey`)
const ConcatStore = Context.Service("test/Real" + "ConcatKey")
const templateUnusedHelper = () => "templateUnused"
const TemplateLayer = Layer.effect(
  TemplateStore,
  Effect.gen(function* () {
    const readTemplate = () => "readTemplate"
    const templateUnused = () => templateUnusedHelper()
    return TemplateStore.of({ readTemplate, templateUnused })
  })
)
const concatUnusedHelper = () => "concatUnused"
const ConcatLayer = Layer.effect(
  ConcatStore,
  Effect.gen(function* () {
    const readConcat = () => "readConcat"
    const concatUnused = () => concatUnusedHelper()
    return ConcatStore.of({ readConcat, concatUnused })
  })
)

const program = Effect.gen(function* () {
  return [(yield* TemplateStore).readTemplate(), (yield* ConcatStore).readConcat()]
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(TemplateLayer, ConcatLayer))),
  ["readTemplate", "readConcat"]
)
