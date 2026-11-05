import assert from "node:assert"
import * as Stripe from "@distilled.cloud/stripe"
import { CreateCustomer } from "@distilled.cloud/stripe/stripe"
import * as Effect from "effect/Effect"
import * as HttpClient from "effect/http/HttpClient"
import * as HttpClientResponse from "effect/http/HttpClientResponse"
import * as Layer from "effect/Layer"
import * as Redacted from "effect/Redacted"

// The generated Stripe client from @distilled.cloud/stripe 1.0.0-rc.18, with one
// operation called against a deterministic mock transport.
const requests = []

const MockHttpClient = Layer.succeed(
  HttpClient.HttpClient,
  HttpClient.make((request) => {
    requests.push(`${request.method} ${request.url}`)
    return Effect.succeed(HttpClientResponse.fromWeb(
      request,
      new Response(JSON.stringify({ id: "cus_test", object: "customer", email: "ops@example.com" }), {
        status: 200,
        headers: { "content-type": "application/json" }
      })
    ))
  })
)

const Live = Layer.mergeAll(
  MockHttpClient,
  Stripe.credentials({ apiKey: Redacted.make("sk_test"), apiBaseUrl: "https://mock.invalid" }),
  Stripe.StripeProtocol
)

const customer = await Effect.runPromise(CreateCustomer({ email: "ops@example.com" }).pipe(Effect.provide(Live)))
assert.deepStrictEqual(customer, { id: "cus_test", object: "customer", email: "ops@example.com" })
assert.deepStrictEqual(requests, ["POST https://mock.invalid/v1/customers"])
