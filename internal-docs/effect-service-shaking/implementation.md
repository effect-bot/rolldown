# Effect service shaking — Implementation

> The rationale and principles behind this live in [design.md](./design.md).

## Summary

The pass is one link-stage step, `LinkStage::effect_service_shaking`. It is called from `link_stage/mod.rs` after `cross_module_optimization` and before `include_statements`. It binds the Effect model to the bundle's `effect` modules and generates constraints for every other module. Then it solves one worklist, turns per-key demand into property removals in the module ASTs, and patches `StmtInfo` so Rolldown's tree shaking drops what the removed methods were the only users of. It is enabled by `experimental.effectServiceShaking` and does nothing otherwise.

## Files

All of it is in `crates/rolldown/src/stages/link_stage/effect_service_shaking/`:

| File          | Role                                                                                                                                                                                                                                                                                                       |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`      | Orchestration: find `effect`, check the major (`effect::SUPPORTED_MAJOR`) and the `MODULES` exports, bind rules, generate, solve, prune, report. Emits the `EFFECT_SERVICE_SHAKING` warnings.                                                                                                              |
| `effect.rs`   | The Effect model, and the only file that names an Effect API. `MODULES` lists the public modules and their ruled exports (`Spec`). `register` installs the rules (`effect:Effect.map`, `effect:Layer.succeed`, …), the never-prune keys and the `effect:default` rule.                                     |
| `generate.rs` | Constraint generation from oxc ASTs: JavaScript semantics only. Top-level code is installed at once. A function body's constraints wait in `FnInfo::pending` until the solver finds the function reachable. Also does constant-string folding (`+`, templates, `.join`), TDZ facts and inert `try` blocks. |
| `heap.rs`     | Abstract objects (`ObjKind`), variables, `Constraint`s and the worklist solver. Demand, bails, the budget and per-call-site cloning (`clone_for`) live here.                                                                                                                                               |
| `host.rs`     | Models of builtins and host APIs (`Object.*`, `Array.prototype.*`, `Promise`, `Map`/`Set`, Node modules). A missing name is an opaque callee.                                                                                                                                                              |
| `prune.rs`    | From demand to edits: method eligibility (`is_method`), purity and TDZ checks, AST mutation, removal of dead locals, and the `StmtInfo` patch.                                                                                                                                                             |

The diagnostic is `EventKind::EffectServiceShaking` (`crates/rolldown_error/src/build_diagnostic/events/effect_service_shaking.rs`), built with `BuildDiagnostic::effect_service_shaking(reason)`. It is gated by `checks.effectServiceShaking`.

## Data flow

1. **Find and bind.** For every `MODULES` entry, the pass resolves the public `effect/<Module>` module in the graph and its exports. If `effect` is external, or a ruled export is missing, or the major is not `SUPPORTED_MAJOR`, it warns and returns. The modelled set is the public modules plus the owners of their ruled exports' `alias_chain`s, so `Effect.map`'s rule lands on `internal/effect.js`'s `map`. Every export of a modelled module is bound to one of three things, in `Shared::rule_symbols`: its `Spec` object, a declared Key, or `effect::default_object` (the `effect:default` rule). An export that aliases ordinary code is linked to that code instead.
2. **Generate.** Each non-modelled module's AST is walked once by `Gen`. Modelled modules are skipped entirely: their bodies sit behind rules.
3. **Solve.** `Analysis::solve` runs the worklist to a fixpoint. Calls to a builtin object run its rule. Calls to a closure activate the function and install its pending constraints. A function marked `cloneable` is cloned instead, once per call site. Cloneable means a leaf `Normal`/arrow function with at most `MAX_CLONE_PENDING` pending constraints, marked by `Gen::mark_cloneable`. Clones are capped at `MAX_CLONES_PER_FN` per function and `MAX_CLONE_DEPTH` levels. A clone renames its own variables and locals, tags its call sites with `CLONE_SUB` and shares everything it captures. A wrap (`Analysis::wrap_of`) calls its callbacks from a sub-site of the rule call that made it, so builtin callbacks get one rule invocation per wrap. Closures handed to wraps share one clone per function (`WRAP_CLONE_SITE`); a clone per wrap exhausts the budget. `over_budget` checks the object, variable and points-to entry counts on every round. Over budget, `bail_everything` runs and every method is kept.
4. **Demand.** A read on the value of key `K` adds the name to `K`'s demand. Unknown names, enumeration, escapes to opaque code and reflection on a Context, Layer or Runtime internal bail the key, or every key the object may hold. `Analysis::key_is_bailed` also honours `never_prune` and `bail_all`.
5. **Prune.** `prune.rs` considers only object literals that reach a key's value, directly or through spread sources. Their method properties not in the demand set are removed, provided the initialiser is pure and TDZ-safe. Local `const`/`let` bindings that only removed properties used are deleted. Then `StmtInfo.referenced_symbols` is patched. Top-level statements are never removed by the pass.

## Debugging

Environment variables, all opt-in:

- `ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT=1` prints the per-key summary (providers, removed and kept methods, bail reasons), plus the solve statistics.
- `ROLLDOWN_EFFECT_SERVICE_SHAKING_DEBUG=1` adds phase timings, the module index table, solver histograms during the solve (loads by name, calls by module, the fattest variables).
- `ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_{OBJECTS,VARS,ENTRIES}` override the budget (defaults 3M objects, 30M variables, 16M points-to entries).

## Tests

- Fixtures: `crates/rolldown/tests/rolldown/tree_shaking/effect_service/` (116 real-`effect` programs). The README there has one row per fixture with its status. Run them with `cargo test --profile effect-verify -p rolldown --test integration -- tree_shaking__effect_service`.
- Acceptance: `crates/rolldown/tests/rolldown/topics/effect_service_acceptance/` builds each program in `programs/` with the pass off and on. It checks behaviour, removed names and the size reduction. `solve_is_deterministic` there builds `node_file_system` five times and requires the same bundle and the same solve (the variables, objects, solver steps and removals of the pass's `effect service shaking` debug event).
- Timing: `cargo run --profile effect-verify -p rolldown --example effect_service_shaking -- <entry> <out-dir> 7` alternates off/on builds and prints medians.
- Every run is under `rolldown_testing::memory_guard` (`ROLLDOWN_TEST_MAX_RSS_MB`).

## Current status

- Fixtures: 114 of 116 pass. `dynamic_key_prefix` waits for prefix narrowing. `effect_internal_analysed` keeps every method; see design.md, "Precision on Schema-heavy programs".
- Acceptance: `node_file_system`, the library and mixed builds pass. `openai_generated_client` and `distilled_stripe` keep every method. OpenAI goes over its budget. Distilled reads Schema fields on the ambient Context, and goes over its budget in some builds. Both are also far over the time target: the solve takes about 9 s on OpenAI and 10 s on Distilled, against a 247 ms OpenAI build without the pass.
- Determinism: `solve_is_deterministic` is red. The bundle and the variable, object and removal counts agree between builds of `node_file_system`, but the solver step count differs (about 178,600 to 183,200). On `effect_internal_analysed` and `distilled_stripe` the variable count differs by up to 18% between builds and the budget verdict flips, so a program near the budget can remove methods in one build and keep everything in the next. Module indices also differ between builds (the debug report names the same module by different indices), which is one candidate source.

## Related

- [design.md](./design.md) — the principles and trade-offs behind this
- `../linking/` — the link stage the pass runs in
