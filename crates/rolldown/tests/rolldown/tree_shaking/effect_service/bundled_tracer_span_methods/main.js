import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';
import * as Tracer from 'effect/Tracer';

// A Tracer written in bundled code, provided by a Layer. Effect calls the
// `attribute` and `end` methods of the spans it returns, and those call a
// captured service's methods. Both methods stay; the unread one is removed.
const Recorder = Context.Service('test/TracerRecorder');
const recorded = [];
const RecorderLayer = Layer.succeed(Recorder)({
  onAttribute: (key) => recorded.push(`attribute ${key}`),
  onEnd: (name) => recorded.push(`end ${name}`),
  recorderUnused: () => recorded.push('unused'),
});
const TracerLayer = Layer.effect(Tracer.Tracer)(
  Effect.gen(function* () {
    const recorder = yield* Recorder;
    return Tracer.make({
      span: (options) => ({
        _tag: 'Span',
        name: options.name,
        spanId: 'span',
        traceId: 'trace',
        parent: options.parent,
        annotations: options.annotations,
        status: { _tag: 'Started', startTime: options.startTime },
        attributes: new Map(),
        links: options.links,
        sampled: options.sampled,
        kind: options.kind,
        end: () => {
          recorder.onEnd(options.name);
        },
        attribute: (key) => {
          recorder.onAttribute(key);
        },
        event: () => {},
        addLinks: () => {},
      }),
    });
  }),
).pipe(Layer.provide(RecorderLayer));
const program = Effect.annotateCurrentSpan('answer', 42).pipe(
  Effect.as(Option.some('done')),
  Effect.withSpan('traced'),
);
assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, TracerLayer)),
  Option.some('done'),
);
assert.deepStrictEqual(recorded, ['attribute answer', 'end traced']);
