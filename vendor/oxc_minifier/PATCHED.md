# Patched oxc_minifier 0.157.0

This is the crates.io release of `oxc_minifier` 0.157.0 (the version `oxc`
0.157.0 uses) with one fix. The root `Cargo.toml` substitutes it via
`[patch.crates-io]`. Its package metadata drops the examples and sets
`publish = false`. Nothing else changed.

## The fix: remove a dead declaration chain in one pass

Dead-code elimination removed one link per compression pass from a chain of
unused declarations whose links call earlier links:

```js
const link0 = () => console.log('unreachable');
const link1 = () => link0();
// ...
const link11 = () => link10();
```

`minimize_statements` decides each declaration in source order. When it
reaches `link10`, the reference from `link11` is still live, and it is only
pruned when the pass is flushed. So each pass removed the last link. A chain of
12 or more links kept the fixed-point loop in `Compressor::run_in_loop`
changing past its guard, so `debug_assert!(false, "Ran loop more than 10
times.")` fired. Release builds stopped early instead, leaving part of the
chain behind. Function declarations were not affected, because the
recursive-function graph (`symbol_liveness`) clears their chains in one
analysis. The bug affects `const`/`let`/`var` declarators whose
initializers are arrows, function expressions, object literals with methods,
or default parameters. Bundling any `effect/http/HttpServerResponse` import
hit it.

The patch has two parts:

- `peephole/remove_unused_declaration.rs`: `symbol_is_unused_by_count` and
  `self_recursive_function_declarator_is_unused` no longer count references
  dropped earlier in the current pass. These are the references in
  `PassChanges::removed_references`. A dropped reference's node is gone for
  good, because the bitset is never unset and the debug over-prune guard in
  `compression_pass` checks this at flush. References created during the pass
  lie beyond the bitset and still count as live. With this change, chains
  whose links call later links go in one forward walk.
- `peephole/minimize_statements.rs`: `remove_declarations_orphaned_this_pass`
  walks the block's statements once more, last to first, after the forward
  walk. It removes function declarations, and declarators whose initializer is
  absent or has no side effects, when every reference to them was dropped this
  pass. Each removal orphans the link declared before it, so a chain whose
  links call earlier links goes in the same pass. Declarations whose
  initializer still has side effects are left for the next pass's forward
  walk, which keeps those side effects.

The iteration limit, the debug assertion and the recursive-function graph's
candidate rules are unchanged.

The regression test is `crates/rolldown/tests/rolldown/dce/dead_arrow_chain`.

`upstream.patch` is the same change against oxc's `crates_v0.157.0` tag, with
tests. Upstream's `cargo test -p oxc_minifier` passes with it: 748 tests,
plus two new ones for chains in either direction, object methods, default
parameters and mixed declarations, and for kept side effects.
`recursive_function_var_redeclaration_converges_on_count_pass` changes its
expectations. `function f() { f() } var f;` now converges in one pass, not
two, and a capped run removes `var f` as well. `tasks/minsize` output is
byte-identical for all 12 libraries; antd.js needs 3 passes instead of 4.

## Removing this patch

The fix is upstreamable as is. Once an `oxc` release contains an equivalent
fix:

1. Delete the `[patch.crates-io]` entry, the `exclude` entry in the root
   `Cargo.toml`, and this directory.
2. Bump `oxc`.
3. Keep `dead_arrow_chain` green.

When bumping `oxc` before then, rebase the patch onto the new
`oxc_minifier`. The patched crate must keep the same version as the one
`oxc` uses, or Cargo ignores the patch.
