import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { build } from "./client.js"

// The function that builds the provider's object is also called directly, and
// that object's methods are read by a name known only at run time. Removing a
// property from the shared literal would break the direct caller, so it stays.
class Client extends Context.Service()("test/RealProviderConstructorShared") {}

const make = Effect.fnUntraced(function* () {
  return build()
})

const ClientLayer = Layer.effect(Client, make())

const program = Effect.gen(function* () {
  const client = yield* Client
  const direct = build()
  const name = ["un", "used"].join("")
  return [client.get(), direct[name]()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, ClientLayer)), ["got", "unused"])
