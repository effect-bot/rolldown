import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Formatter from 'effect/Formatter';
import * as Inspectable from 'effect/Inspectable';
import * as Layer from 'effect/Layer';
import * as Redactable from 'effect/Redactable';

// Services passed to Effect APIs whose parameter is typed `unknown`: the API
// still calls their protocol methods. Inspectable.toJson calls toJSON,
// and Redactable.redact and Formatter.format call [Redactable.symbolRedactable].
// The program calls one method of each service itself. Those methods and the
// protocol methods stay; each service's unread method is removed.
const Json = Context.Service('test/UnknownParamJson');
const Redacted = Context.Service('test/UnknownParamRedacted');
const layer = Layer.mergeAll(
  Layer.succeed(Json)({
    jsonUsed: () => 'json used',
    toJSON: () => 'json protocol',
    jsonUnused: () => 'unused',
  }),
  Layer.succeed(Redacted)({
    redactedUsed: () => 'redacted used',
    [Redactable.symbolRedactable]: () => 'redacted protocol',
    redactedUnused: () => 'unused',
  }),
);
const program = Effect.gen(function* () {
  const json = yield* Json;
  const redacted = yield* Redacted;
  return [
    json.jsonUsed(),
    redacted.redactedUsed(),
    Inspectable.toJson(json),
    Redactable.redact(redacted),
    Formatter.format(redacted),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'json used',
  'redacted used',
  'json protocol',
  'redacted protocol',
  '"redacted protocol"',
]);
