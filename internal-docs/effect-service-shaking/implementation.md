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

1. **Find and bind.** For every `MODULES` entry, the pass resolves the public `effect/<Module>` module in the graph and its exports. If `effect` is external, or a ruled export is missing, or the major is not `SUPPORTED_MAJOR`, it warns and returns. The modelled set is the public modules plus the owners of their ruled exports' `alias_chain`s. The binding walks modules in execution order, but precedence does not depend on it (`BindRank`): an explicit rule beats a declared Key, which beats the default. At equal rank an export's own declaration takes its spec and the declarations it aliases keep what claimed them first. So `internal/effect.js`'s `map`, walked before `Effect.js`, first takes the default as that module's own export, and `Effect.map`'s rule then replaces it. Ordinary code that imports `internal/effect.js` directly, as Schema does, hits the stable rule. Every export of a modelled module is bound to one of three things, in `Shared::rule_symbols`: its `Spec` object, a declared Key, or `effect::default_object` (the `effect:default` rule). An export that aliases ordinary code is linked to that code instead.
2. **Generate.** Each non-modelled module's AST is walked once by `Gen`. Modelled modules are skipped entirely: their bodies sit behind rules.

   Every walk over modules (namespace objects, binding, generation, namespace exports) goes in execution order (`exec_order`, ties by `stable_id`), and every walk over a module's exports goes in name order (`sorted_exports`). `ModuleIdx` follows the order modules finished loading, which differs between builds of the same input, and `resolved_exports` is a hash map. The walk order matters because the solve is first-come: the first `MAX_CLONES_PER_FN` call sites to reach a function get clones and later ones share the body, and later calls of a builtin at one site merge into its first invocation (`rule_memo`). Another order gives another sound but differently precise constraint set, and can go over the budget where this one does not.

3. **Solve.** `Analysis::solve` runs the worklist to a fixpoint. Calls to a builtin object run its rule. Calls to a closure activate the function and install its pending constraints. `new F(...)` on a function constructor makes one instance per function, with `F.prototype` as its prototype, and runs the body with that instance as `this`. An ordinary function's `prototype` is a default object (`Analysis::function_prototype`), whose `constructor` is the function, until code replaces it. A class's prototype has `constructor` set to the class. `super.m()` looks `m` up on the parent prototype (the parent class itself in static code) and calls it with the current `this`. A class getter or setter runs with `this` holding its declaration target and every object that inherits from it, directly or through a chain (`Constraint::Inherits`, tracked when an object's prototype variable is created, and `Constraint::Accessor`); a setter's parameter also receives what is written under its name on those objects. Accessors therefore run once for all their receivers, not once per receiver. `new C(...)` on a class makes one instance per class and runs `Analysis::construct_on` on it. A class with a constructor runs it; `super(...)` in it runs the parent constructor on the same `this` with `super`'s own arguments, and `this` then also holds what the parent yielded. A derived class without a constructor runs its parent's construction on the instance with the `new` call's arguments, as JavaScript's implicit constructor does. Every level passes up the objects a constructor returns, so a replacement object reaches the `new` result and the derived `this`. The instance stays among the results. A function marked `cloneable` is cloned instead, once per call site. Cloneable means a leaf `Normal`/arrow function with at most `MAX_CLONE_PENDING` pending constraints, marked by `Gen::mark_cloneable`. Clones are capped at `MAX_CLONES_PER_FN` per function and `MAX_CLONE_DEPTH` levels. A clone renames its own variables and locals, tags its call sites with `CLONE_SUB` and shares everything it captures. A wrap (`Analysis::wrap_of`) calls its callbacks from a sub-site of the rule call that made it, so builtin callbacks get one rule invocation per wrap. Closures handed to wraps share one clone per function (`WRAP_CLONE_SITE`); a clone per wrap exhausts the budget. `over_budget` checks the object, variable and points-to entry counts on every round. Over budget, `bail_everything` runs and every method is kept.
4. **Demand.** A read on the value of key `K` adds the name to `K`'s demand. Unknown names, enumeration, escapes to opaque code and reflection on a Context, Layer or Runtime internal bail the key, or every key the object may hold. `Analysis::key_is_bailed` also honours `never_prune` and `bail_all`.
5. **Prune.** `prune.rs` considers only object literals that reach a key's value, directly or through spread sources. Their method properties not in the demand set are removed, provided the initialiser is pure and TDZ-safe. Local `const`/`let` bindings that only removed properties used are deleted. Then `StmtInfo.referenced_symbols` is patched. Top-level statements are never removed by the pass.

## Debugging

Environment variables, all opt-in:

- `ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT=1` prints the per-key summary (providers, removed and kept methods, bail reasons), plus the solve statistics.
- `ROLLDOWN_EFFECT_SERVICE_SHAKING_DEBUG=1` adds phase timings, the module index table, solver histograms during the solve (loads by name, calls by module, the fattest variables).
- `ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_{OBJECTS,VARS,ENTRIES}` override the budget (defaults 3M objects, 30M variables, 16M points-to entries).

## Tests

- Fixtures: `crates/rolldown/tests/rolldown/tree_shaking/effect_service/` (153 real-`effect` programs). The README there has one row per fixture with its status. Run them with `cargo test --profile effect-verify -p rolldown --test integration -- tree_shaking__effect_service`.
- Acceptance: `crates/rolldown/tests/rolldown/topics/effect_service_acceptance/` builds each program in `programs/` with the pass off and on. It checks behaviour, removed names and the size reduction. `solve_is_deterministic` there builds `node_file_system` five times and requires the same bundle and the same solve (the variables, objects, solver steps and removals of the pass's `effect service shaking` debug event).
- Timing: `cargo run --profile effect-verify -p rolldown --example effect_service_shaking -- <entry> <out-dir> 7` alternates off/on builds and prints medians.
- Every run is under `rolldown_testing::memory_guard` (`ROLLDOWN_TEST_MAX_RSS_MB`).

## Current status

- Escape rules (issue, "Code the pass can't see"): `Analysis::escape` hands over ordinary values only. A service reached through them keeps every method (`demand.all`), and later stores into an escaped object escape too. Effect's containers, keys and Effect primitives escape as nothing, a module namespace escapes as its exports, and an effect built by unseen code runs without a bail. For `o.m()` into unseen code, `Constraint::EscapeReceiver` hands over only the receivers whose own `m` slot can hold that code (`EscapeIfOpaque`), or that have no own `m`. The call's property name comes from `Analysis::method_names`, filled by the generator; clone sites are not listed and hand over every receiver. A key whose values include an external value no longer counts as fully demanded (`prune.rs`).
- Public types (issue, "Assumptions" 2): `Analysis::exits` marks the model Effects whose public type is `Exit`, and only those resolve `Exit.{_tag,value,cause}`. `Effect.exit`, `onExit`, `runSyncExit`/`runPromiseExit` (both forms), `Fiber.await`, `addObserver`/`pollUnsafe` and `acquireUseRelease`'s release produce Exits rather than Wraps. `ManagedRuntime`'s remaining public members (`dispose`, `disposeEffect`, `scope`) have rules, so reading them is no longer reflection.
- Fixtures: 133 of 153 pass.
  - `dynamic_key_prefix` waits for prefix narrowing.
  - `exit_view_of_effect` and `effect_unruled_member`: the pass removes both methods (`removed=2`), but each `_test.mjs` asserts the method names appear nowhere, and the program's own call site (`eff.value.probeUsed()`, `….value.value.unruledUsed()`) stays in the bundle. The checks need to match the definitions instead; that is a tests-only fix.
  - The class-semantics fixtures `computed_setter_receiver`, `super_accessor_borrowed_receiver` and `nested_class_computed_super` still fail (the bundle throws). That work is paused.
  - `receiver_helper_separation`, `constructor_receiver_separation`, `derived_constructor_separation`, `heap_factory_separation` and `new_factory_separation` keep the unread method because one body or allocation serves every receiver or caller.
  - `effect_internal_analysed`, `schema_concurrency_no_fiber` and `schema_concurrency_context` go over the entries budget.
  - `internal_combinator_caller`, `retry_schedule_callback`, `retry_or_else_schedule_callback`, `retry_or_else_schedule_output`, `retry_metadata_output` and `retry_or_else_source_defect` run correctly but keep the unread method (Schedule/Duration merging, or the Pull failure sharing a slot with the defect).
- Acceptance: `node_file_system` and both mixed setups pass. `mixed_effect_bundled_library_external` now removes `appUnused` (0.1% smaller). `openai_generated_client` and `distilled_stripe` go over the entries budget and are 0.0% smaller; design.md, "What blocks the OpenAI and Stripe gates", has the trace.
- Determinism: `solve_is_deterministic` passes.
- Measurements against `1129245ac` (example binary, one build, `MemoryMax=6G` and `ROLLDOWN_TEST_MAX_RSS_MB=4096`):

  | program                    | `1129245ac` vars / entries / solve / verdict | now                                      |
  | -------------------------- | -------------------------------------------- | ---------------------------------------- |
  | `node_file_system`         | 86k / 0.25M / 101 ms / within, 31 removed    | 86k / 0.21M / 91 ms / within, 31 removed |
  | `effect_internal_analysed` | 335k / 16.0M / 4.3 s / over                  | 295k / 16.0M / 5.9 s / over              |
  | `distilled_stripe`         | 295k / 16.0M / 19.2 s / over                 | 299k / 16.0M / 32.4 s / over             |
  | `openai_generated_client`  | 412k / 16.1M / 11.4 s / over                 | 379k / 16.0M / 5.4 s / over              |

  Entries are points-to entries plus copy edges at the end of the solve; an over-budget solve stops at the limit.

## Related

- [design.md](./design.md) — the principles and trade-offs behind this
- `../linking/` — the link stage the pass runs in
