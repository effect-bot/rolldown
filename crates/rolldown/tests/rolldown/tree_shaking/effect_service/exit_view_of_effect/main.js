import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';

// `Effect.succeed(svc)` is an Exit at run time. The program reads the service
// back through the public Exit view (`Exit.isExit`, `Exit.isSuccess`,
// `.value`), which the model has no rule for: that read must count.
const Probe = Context.Service('test/ExitViewProbe');

const ProbeLayer = Layer.succeed(
  Probe,
  Probe.of({ probeUsed: () => 'used', probeUnused: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const svc = yield* Probe;
  const eff = Effect.succeed(svc);
  if (Exit.isExit(eff) && Exit.isSuccess(eff)) return eff.value.probeUsed();
  return 'not an exit';
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, ProbeLayer)), 'used');
