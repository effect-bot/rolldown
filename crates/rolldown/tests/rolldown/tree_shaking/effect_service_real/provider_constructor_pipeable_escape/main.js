import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { build } from "./client.js"

// An arbitrary pipeable of the constructor receives the Effect that builds the
// object and can hand the object to user code, here reading a method by a name
// known only at run time. The constructor is not followed, so it stays whole.
class Client extends Context.Service()("test/RealProviderConstructorPipeableEscape") {}

const observed = []

const make = Effect.fnUntraced(
  function* () {
    return build()
  },
  (effect) => Effect.tap(effect, (client) => Effect.sync(() => observed.push(client[["un", "used"].join("")]())))
)

const ClientLayer = Layer.effect(Client, make())

const program = Effect.gen(function* () {
  const client = yield* Client
  return client.get()
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, ClientLayer)), "got")
assert.deepStrictEqual(observed, ["unused"])
