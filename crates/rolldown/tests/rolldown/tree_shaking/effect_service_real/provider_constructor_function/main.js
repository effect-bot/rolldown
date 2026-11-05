import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { build } from "./client.js"

// The shape of a generated client: a class-form key, provided by
// `Layer.effect(Key, make(options))`, where `make` is an `Effect.fnUntraced`
// generator returning the object built by a function in another module. The
// piped argument runs while the service is constructed.
class Client extends Context.Service()("test/RealProviderConstructorClient") {}
class Http extends Context.Service()("test/RealProviderConstructorHttp") {}

const log = []

const make = Effect.fnUntraced(
  function* (options) {
    const http = yield* Http
    log.push(`make ${options.base}`)
    return build({ request: (path) => http.request(`${options.base}${path}`) })
  },
  (effect) => Effect.tap(effect, () => Effect.sync(() => log.push("piped")))
)

const HttpLayer = Layer.succeed(Http, { request: (url) => `GET ${url}` })
const ClientLayer = Layer.effect(Client, make({ base: "mock:" })).pipe(Layer.provide(HttpLayer))

const program = Effect.gen(function* () {
  const client = yield* Client
  return client.get("/models")
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, ClientLayer)), "GET mock:/models")
assert.deepStrictEqual(log, ["make mock:", "piped"])
