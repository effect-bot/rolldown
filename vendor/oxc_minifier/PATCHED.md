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
  removes, after the forward walk, every declaration of the block whose
  references were all dropped this pass, until no removal orphans another.
  It first walks the block's declarations once, last to first, which clears
  a chain whose links call earlier links. Each removal records the symbols
  its initializer or body referenced. If the walk removed anything, those
  symbols' declarations in the block are checked again through a worklist,
  so a chain declared in any order goes in the same pass, and each
  declaration is rechecked only when a removal may have orphaned it. A
  removed declarator whose initializer has side effects leaves them where it
  was, reduced by `remove_unused_expression` as in the forward walk; the
  declaration is split around it, so `const a = 1, b = (f(), () => a), c = 2`
  becomes `const a = 1; f(); const c = 2`, and side effects run in the same
  order. The removed statements and declarators are taken out in one rebuild
  of the block at the end. `remove_unused_function_declaration`'s check moved
  into `function_declaration_is_unused`, so the closure can check before it
  walks a function.

The iteration limit, the debug assertion and the recursive-function graph's
candidate rules are unchanged.

The regression tests are in `crates/rolldown/tests/rolldown/dce/`, each in
`minify: 'dceOnly'` and `minify: true`: `dead_arrow_chain` (links calling
earlier links), `dead_chain_mixed_order` (24 links declared 0, 2, …, 22, 23,
21, …, 1) and `dead_chain_effectful_inits` (12 links
`(tick(n), () => link(n-1)())`, whose ticks must all run, in order). The
last two used to go over the pass limit, with and without the first version
of this patch.

`upstream.patch` is the same change against oxc's `crates_v0.157.0` tag, with
tests. Upstream's `cargo test -p oxc_minifier` passes with it (750 passed, 26
ignored), and `cargo clippy -p oxc_minifier --all-targets -- -D warnings` is
clean. The new tests cover chains in either direction, out of order, through
object methods, default parameters and mixed function and `var`
declarations, and chains with effectful initializers, whose effects stay in
order, including a declaration split around one. A 12-link effectful chain
takes two passes, as one effectful dead declarator already does; the second
only settles the kept expression.
`recursive_function_var_redeclaration_converges_on_count_pass` changes its
expectations. `function f() { f() } var f;` now converges in one pass, not
two, and a capped run removes `var f` as well. `tasks/minsize` sizes, minified
and gzipped, are unchanged for all 12 libraries; antd.js needs 3 passes
instead of 4.

## Removing this patch

The fix is upstreamable as is. Once an `oxc` release contains an equivalent
fix:

1. Delete the `[patch.crates-io]` entry, the `exclude` entry in the root
   `Cargo.toml`, and this directory.
2. Bump `oxc`.
3. Keep `dead_arrow_chain`, `dead_chain_mixed_order` and
   `dead_chain_effectful_inits` green.

When bumping `oxc` before then, rebase the patch onto the new
`oxc_minifier`. The patched crate must keep the same version as the one
`oxc` uses, or Cargo ignores the patch.
