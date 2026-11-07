import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Redacted from 'effect/Redacted';
import * as HttpApiMiddleware from 'effect/http-api/HttpApiMiddleware';
import * as HttpApiSecurity from 'effect/http-api/HttpApiSecurity';

// A user class extending `HttpApiMiddleware.Service`, which returns its own
// subclass of a `Context.Service` class. Only direct `Context.Service` and
// `Context.Reference` classes are service keys, so neither scheme handler may
// be removed, although the program calls only `guardUsed`. The handler reads
// `Tokens`, a direct key, so its `check` must stay too: the handlers run
// although no key names their service.
class Tokens extends Context.Service()('test/HttpApiMiddlewareServiceKept/Tokens') {}
class Guard extends HttpApiMiddleware.Service()('test/HttpApiMiddlewareServiceKept', {
  security: {
    guardUsed: HttpApiSecurity.bearer,
    guardUnused: HttpApiSecurity.apiKey({ key: 'x-guard' }),
  },
}) {}

const TokensLayer = Layer.succeed(Tokens, { check: (token) => token === 'token' });

const GuardLayer = Layer.effect(
  Guard,
  Effect.gen(function* () {
    const tokens = yield* Tokens;
    return {
      guardUsed: (httpEffect, { credential }) =>
        Effect.map(
          httpEffect,
          () => `guard used marker ${tokens.check(Redacted.value(credential))}`,
        ),
      guardUnused: (httpEffect) => Effect.map(httpEffect, () => 'guard unused marker'),
    };
  }),
);

// The handler is called directly, with a plain response value and no endpoint
// or group: the untyped program needs no HTTP modules beyond the middleware's.
const program = Effect.gen(function* () {
  const guard = yield* Guard;
  return yield* guard.guardUsed(Effect.succeed('response'), {
    credential: Redacted.make('token'),
  });
});

assert.strictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.provide(GuardLayer, TokensLayer))),
  'guard used marker true',
);
