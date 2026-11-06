import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';

// The fiber succeeds with the service, and an observer added with the public
// `addObserver` reads it from the Exit it receives, narrowed with
// `Exit.isSuccess`. This type checks against Effect's public types, so it is a
// real flow the pass must follow.
const Probe = Context.Service('test/FiberObserverProbe');

const ProbeLayer = Layer.succeed(
  Probe,
  Probe.of({ probeObserved: () => 'observed', probeUnused: () => 'unused' }),
);

const fiber = Effect.runFork(Effect.provide(Effect.service(Probe), ProbeLayer));
const result = await new Promise((resolve) => {
  fiber.addObserver((exit) =>
    resolve(Exit.isSuccess(exit) ? exit.value.probeObserved() : 'failed'),
  );
});

assert.strictEqual(result, 'observed');
