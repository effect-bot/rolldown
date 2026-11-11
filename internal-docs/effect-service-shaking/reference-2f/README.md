# Lane 2F reference (quarantined)

This branch keeps lane 2F as written and verified: `5b663cbe` (red tests), `3f28a273` (Host arms in `store_name` and `spread_into`) and `03ccb1e1` (docs). The Architect quarantined it: the Host arm makes the host the first escape for some values, and `escape_with`'s early return then skips their later escape into effect's code or unseen code (on default `distilled_stripe`, about 22 values lose the effect escape and 18 the genuine upgrade). The delivery branch `effect-service-shaking-stripe` reverts all three commits. Tim excluded the Proxy handler scenario (`host_proxy_literal_handler`); the wider Host late-arrival and escape-order questions stay open for the combined escape batch.

Scratch evidence kept here, outside the fixture directory so no suite runs it. To run a witness, copy its directory into `crates/rolldown/tests/rolldown/tree_shaking/effect_service/` and touch `crates/rolldown/tests/integration/rolldown_fixture.rs`.

| Witness            | Shape                                                                                           | Result at `6b2752f3` and here                                                                                                        |
| ------------------ | ----------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `zz_fl` (lane 2)   | A host call (`zzHost(svc)`, an unresolved global) before the entry export's Ext escape of `svc` | `removed=1 kept=1`; the test throws (TypeError on `flUnused`). Reconstructed from the lane 2 report; the scratch directory was gone. |
| `zz_eh5` (lane 2E) | An external `register(lookup)`, then `zzHost(a.x.y.z)` on the same function                     | `removed=2 kept=0`; the program throws (TypeError)                                                                                   |
| `zz_eh6` (lane 2E) | `export { lookup }`, then `zzHost` on it                                                        | `removed=2 kept=0`; the program throws (TypeError)                                                                                   |

2F does not change these three: they are the escape-order defect (the first escape flavour wins and a later one is skipped).

Counters (uncommitted instrumentation, never landed):

- `counter.patch` applies at `6b2752f3`. It prints `ZZC` lines per solve: escape runs and repeat skips by existing state, incoming flavour, object kind and root tag; late-arm firings by target state; and the default-Host unresolved names. It leaves entries, steps, solves and bundle bytes unchanged on all four gates in both modes.
- `counter-fix.patch` is the same counter on top of `03ccb1e1`, with the two Host arms tagged `@late-store-host` and `@late-spread-host`.
