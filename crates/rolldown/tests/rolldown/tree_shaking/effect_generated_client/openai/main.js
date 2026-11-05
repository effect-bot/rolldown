import assert from "node:assert"
import * as Effect from "effect/Effect"
import * as HttpClient from "effect/http/HttpClient"
import * as HttpClientResponse from "effect/http/HttpClientResponse"
import * as Layer from "effect/Layer"
import * as OpenAiClientGenerated from "@effect/ai-openai/OpenAiClientGenerated"

// The generated OpenAI client from @effect/ai-openai, reached through its
// service, with one method called against a deterministic mock transport.
const requests = []

const MockHttpClient = Layer.succeed(
  HttpClient.HttpClient,
  HttpClient.make((request) => {
    requests.push(`${request.method} ${request.url}`)
    return Effect.succeed(HttpClientResponse.fromWeb(
      request,
      new Response(
        JSON.stringify({ id: "gpt-test", object: "model", created: 1700000000, owned_by: "test" }),
        { status: 200, headers: { "content-type": "application/json" } }
      )
    ))
  })
)

const ClientLayer = OpenAiClientGenerated.layer({ apiUrl: "https://mock.invalid/v1" }).pipe(
  Layer.provide(MockHttpClient)
)

const program = Effect.gen(function* () {
  const client = yield* OpenAiClientGenerated.OpenAiClientGenerated
  return yield* client.retrieveModel("gpt-test", undefined)
})

const model = await Effect.runPromise(Effect.provide(program, ClientLayer))
assert.deepStrictEqual(model, { id: "gpt-test", object: "model", created: 1700000000, owned_by: "test" })
assert.deepStrictEqual(requests, ["GET https://mock.invalid/v1/models/gpt-test"])
