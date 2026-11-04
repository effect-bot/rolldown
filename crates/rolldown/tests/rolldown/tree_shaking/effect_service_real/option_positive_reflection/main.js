import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Supported: user code enumerates, indexes by non-constant names and serializes its own
// data, never an Effect runtime object. Each of these disables the pass by default;
// with effectServiceShakingAssumeNoReflection the pass must still remove `writeOne`.
const Store = Context.Service("test/RealOptionPositive")

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

const settings = { mode: "fast", level: 2 }
const describe = () => {
  const names = Object.keys(settings)
  const values = []
  for (const name in settings) values.push(settings[name])
  const copy = structuredClone(settings)
  return [names.join(","), values.join(","), Reflect.ownKeys(copy).length, JSON.stringify(copy)]
}

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), ...describe()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  "read",
  "mode,level",
  "fast,2",
  2,
  '{"mode":"fast","level":2}'
])
