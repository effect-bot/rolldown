import assert from 'node:assert';
import * as Stripe from '@distilled.cloud/stripe';
import { CreateCheckoutSession, CreateCustomer, GetCustomer } from '@distilled.cloud/stripe/stripe';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as HttpClient from 'effect/http/HttpClient';
import * as HttpClientResponse from 'effect/http/HttpClientResponse';
import * as Layer from 'effect/Layer';
import * as Redacted from 'effect/Redacted';

// Distilled consumed as a service, as in Alchemy: an application service whose
// Layer wraps Distilled operations, provided with Credentials and an
// HttpClient (Alchemy's StripeHttp). The program calls one method.
class Billing extends Context.Service()('app/Billing') {}

const BillingLive = Layer.effect(
  Billing,
  Effect.gen(function* () {
    const createCustomer = (email) => CreateCustomer({ email });
    const retrieveCustomer = (customer) => GetCustomer({ customer });
    const createCheckout = (customer, price) =>
      CreateCheckoutSession({
        mode: 'subscription',
        customer,
        line_items: [{ price, quantity: 1 }],
        success_url: 'https://example.com/welcome',
        cancel_url: 'https://example.com/pricing',
      });
    return Billing.of({ createCustomer, retrieveCustomer, createCheckout });
  }),
);

const requests = [];

const MockHttpClient = Layer.succeed(
  HttpClient.HttpClient,
  HttpClient.make((request) => {
    requests.push(`${request.method} ${request.url}`);
    return Effect.succeed(
      HttpClientResponse.fromWeb(
        request,
        new Response(
          JSON.stringify({ id: 'cus_test', object: 'customer', email: 'ops@example.com' }),
          {
            status: 200,
            headers: { 'content-type': 'application/json' },
          },
        ),
      ),
    );
  }),
);

const StripeLive = Layer.mergeAll(
  MockHttpClient,
  Stripe.credentials({ apiKey: Redacted.make('sk_test'), apiBaseUrl: 'https://mock.invalid' }),
);

const program = Effect.gen(function* () {
  const billing = yield* Billing;
  return yield* billing.createCustomer('ops@example.com');
});

const customer = await Effect.runPromise(
  program.pipe(Effect.provide(BillingLive), Effect.provide(StripeLive)),
);
assert.deepStrictEqual(customer, { id: 'cus_test', object: 'customer', email: 'ops@example.com' });
assert.deepStrictEqual(requests, ['POST https://mock.invalid/v1/customers']);
