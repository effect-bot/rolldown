import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as HttpServerRespondable from 'effect/http/HttpServerRespondable';
import * as HttpServerResponse from 'effect/http/HttpServerResponse';
import * as Layer from 'effect/Layer';

// A service passed to an Effect API whose parameter is typed `unknown`: the API
// still calls its protocol method. HttpServerRespondable.toResponseOrElse calls
// [HttpServerRespondable.symbol]. The program calls one other method itself.
// That method and the protocol method stay; the unread method is removed.
const Responder = Context.Service('test/UnknownParamRespondable');
const layer = Layer.succeed(Responder)({
  respondUsed: () => 'respond used',
  [HttpServerRespondable.symbol]: () => Effect.succeed(HttpServerResponse.empty({ status: 201 })),
  respondUnused: () => 'unused',
});
const program = Effect.gen(function* () {
  const responder = yield* Responder;
  const response = yield* HttpServerRespondable.toResponseOrElse(
    responder,
    HttpServerResponse.empty({ status: 500 }),
  );
  return [responder.respondUsed(), response.status];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'respond used',
  201,
]);
