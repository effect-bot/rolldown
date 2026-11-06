import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';

// `Effect.succeed(svc)` is an Exit at run time, and the program reads the
// service back through `Exit.isExit`, `Exit.isSuccess` and `.value`. On an
// Effect, `.value` is `unknown` in Effect's public types, so the method call
// does not type check: the pass assumes it does not happen (Tim's decision),
// and the read keeps nothing. The valid flow, `Exit.isSuccess` on a real Exit,
// is `fiber_observer_exit`.
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
