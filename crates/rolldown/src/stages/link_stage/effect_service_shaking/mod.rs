//! Removal of Effect service methods that nothing in the bundle can read
//! (`experimental.effectServiceShaking`, EFF-3430).
//!
//! A whole-bundle, inclusion-based value-flow analysis (`heap.rs`, `generate.rs`)
//! with built-in models of JavaScript builtins (`host.rs`) and of the stable
//! primitives of `effect` 4.x (`effect.rs`). Demand is recorded per key and
//! applied to every provider of that key; `prune.rs` turns it into property
//! removals and patches the statement infos so ordinary tree shaking removes
//! what the removed methods were the only users of.
//!
//! Runs after `cross_module_optimization` and before `include_statements`.
//!
//! See `internal-docs/effect-service-shaking/implementation.md` (the how) and
//! `internal-docs/effect-service-shaking/design.md` (the why).

#![expect(clippy::print_stderr, reason = "the opt-in debug report")]

mod effect;
mod generate;
mod heap;
mod host;
mod prune;

use arcstr::ArcStr;
use oxc::ast::ast::{ClassElement, Expression, Statement};
use oxc_index::IndexVec;
use rolldown_common::{ExportsKind, Module, ModuleIdx, Specifier, SymbolRef};
use rolldown_error::{BuildDiagnostic, EventKindSwitcher};
use rustc_hash::{FxHashMap, FxHashSet};

use self::generate::{Gen, ModuleFacts, Shared};
use self::heap::{Analysis, Constraint, Flavour, FnId, ObjKind, Pending};
use super::LinkStage;

impl LinkStage<'_> {
  fn warn_effect_service_shaking(&mut self, reason: String) {
    if self.options.checks.contains(EventKindSwitcher::EffectServiceShaking) {
      self
        .diagnostics
        .push(BuildDiagnostic::effect_service_shaking(reason).with_severity_warning());
    }
  }

  #[tracing::instrument(level = "debug", skip_all)]
  #[expect(clippy::too_many_lines, reason = "one orchestration: bind, generate, solve, prune")]
  pub(super) fn effect_service_shaking(&mut self) {
    if !self.options.experimental.is_effect_service_shaking_enabled() {
      return;
    }
    let start = std::time::Instant::now();
    let debug = std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_DEBUG").is_some();
    macro_rules! phase {
      ($($t:tt)*) => {
        if debug {
          eprintln!("[effect-service-shaking] {} ms: {}", start.elapsed().as_millis(), format!($($t)*));
        }
      };
    }
    let mut a = Analysis::new();
    host::register(&mut a);
    effect::register(&mut a);

    let module_count = self.module_table.modules.len();
    // Module indices follow the order modules finished loading, which differs between builds of
    // the same input, and the solve is order-sensitive (first-come clones and rule invocations).
    // Every walk that allocates variables or objects, or adds constraints, therefore goes in
    // execution order, with exports in name order. See
    // internal-docs/effect-service-shaking/implementation.md (Determinism).
    let mut order: Vec<ModuleIdx> = self.module_table.modules.iter().map(Module::idx).collect();
    order.sort_by(|x, y| {
      let (x, y) = (&self.module_table.modules[*x], &self.module_table.modules[*y]);
      x.exec_order().cmp(&y.exec_order()).then_with(|| x.stable_id().cmp(y.stable_id()))
    });
    let prim = a.var();
    a.seed(prim, a.prim);
    let top_host = a.var();
    a.seed(top_host, a.top_host);
    let top_ext = a.var();
    a.seed(top_ext, a.top_ext);
    let thrown = a.var();
    let mut sh = Shared {
      symbols: &self.symbols,
      symbol_vars: FxHashMap::default(),
      namespace_syms: FxHashMap::default(),
      namespace_objs: FxHashMap::default(),
      rule_symbols: FxHashMap::default(),
      externals: FxHashMap::default(),
      globals: FxHashMap::default(),
      literals: FxHashMap::default(),
      decls: FxHashMap::default(),
      thrown,
      prim,
      top_host,
      top_ext,
      module_fns: IndexVec::from_vec(vec![None; module_count]),
      cjs_modules: FxHashSet::default(),
      lazy_decls: FxHashMap::default(),
      lazy_refs: vec![],
      lazy_by_module: FxHashMap::default(),
      resolve_global: host::resolve_global,
    };

    // == modules: namespaces, externals, CommonJS
    for module in order.iter().map(|idx| &self.module_table.modules[*idx]) {
      match module {
        Module::Normal(normal) => {
          let ns = a.alloc(ObjKind::Namespace, None);
          sh.namespace_objs.insert(normal.idx, ns);
          sh.namespace_syms.insert(
            self.symbols.canonical_ref_for(normal.ecma_view.namespace_object_ref),
            normal.idx,
          );
          if matches!(normal.ecma_view.exports_kind, ExportsKind::CommonJs) {
            sh.cjs_modules.insert(normal.idx);
          }
        }
        Module::External(ext) => {
          let obj = host::external_module(&mut a, ext.id.as_ref());
          sh.externals.insert(ext.idx, obj);
        }
      }
    }

    // == the Effect model, bound to canonical symbols of the public modules
    // The modelled modules are the public modules `MODULES` names, plus the modules their
    // explicitly ruled exports resolve into (`Effect.map` is `internal.map`, so
    // `internal/effect.js` sits behind rules too). Every other module of the package is
    // ordinary code and analysed generically. See design section 3.
    let mut effect_found = false;
    let mut effect_external = false;
    let mut unsupported_version: Option<String> = None;
    let mut public_modules: Vec<(ModuleIdx, &'static [(&'static str, effect::Spec)])> = vec![];
    let mut listed_modules: Vec<(ModuleIdx, &'static [(&'static str, effect::Spec)])> = vec![];
    let mut schedule_module: Option<ModuleIdx> = None;
    for module in order.iter().map(|idx| &self.module_table.modules[*idx]) {
      match module {
        Module::Normal(normal) => {
          let Some(pkg) = normal.originative_resolved_id.package_json.as_ref() else { continue };
          if pkg.name() != Some("effect") {
            continue;
          }
          effect_found = true;
          if let Some(version) = pkg.version() {
            if version.split('.').next() != Some(effect::SUPPORTED_MAJOR) {
              unsupported_version = Some(version.to_string());
            }
          }
          let path = normal.id.as_ref().replace('\\', "/");
          if path.ends_with(effect::SCHEDULE_MODULE) {
            schedule_module = Some(normal.idx);
          }
          if let Some((_, specs)) =
            effect::MODULES.iter().find(|(name, _)| path.ends_with(&format!("/dist/{name}.js")))
          {
            public_modules.push((normal.idx, specs));
          }
          if let Some((_, specs)) = effect::LISTED_MODULES
            .iter()
            .find(|(name, _)| path.ends_with(&format!("/dist/{name}.js")))
          {
            listed_modules.push((normal.idx, specs));
          }
        }
        Module::External(ext) => {
          let id = ext.id.as_ref();
          if id == "effect" || id.starts_with("effect/") {
            effect_external = true;
          }
        }
      }
    }
    // See internal-docs/effect-service-shaking/design.md (diagnostics): when `effect` is external
    // or does not match the model, one warning says so and the pass does no work.
    if effect_external {
      self.warn_effect_service_shaking(
        "`effect` is external, so no Effect primitive can be recognised".to_string(),
      );
      return;
    }
    if !effect_found {
      // No `effect` in the bundle: no service key can exist.
      return;
    }
    if let Some(version) = unsupported_version {
      self.warn_effect_service_shaking(format!(
        "`effect` {version} is not a supported version (the model covers {}.x)",
        effect::SUPPORTED_MAJOR
      ));
      return;
    }
    let missing: Vec<String> = public_modules
      .iter()
      .flat_map(|(module, specs)| {
        let exports = &self.metas[*module].resolved_exports;
        let path = self.module_table[*module].id().to_string();
        specs
          .iter()
          .filter(move |(name, _)| !exports.contains_key(*name))
          .map(move |(name, _)| format!("{}#{name}", path.rsplit('/').next().unwrap_or(&path)))
      })
      .collect();
    if !missing.is_empty() {
      self.warn_effect_service_shaking(format!(
        "`effect` does not match the model, missing exports: {}",
        missing.join(", ")
      ));
      return;
    }
    let mut modelled: FxHashSet<ModuleIdx> = public_modules.iter().map(|(m, _)| *m).collect();
    for (module, specs) in &public_modules {
      for (name, export) in &self.metas[*module].resolved_exports {
        if specs.iter().any(|(n, _)| *n == name.as_str()) {
          for sym in self.alias_chain(export.symbol_ref) {
            modelled.insert(sym.owner);
          }
        }
      }
    }
    // Model-bound declarations that alias ordinary code, linked once every rule is bound.
    let mut links: Vec<(SymbolRef, SymbolRef)> = vec![];
    // Symbols the model or the build reads outside generated code: their lazy statements run.
    let mut lazy_roots: Vec<SymbolRef> = vec![];
    let mut bind_rank: FxHashMap<SymbolRef, BindRank> = FxHashMap::default();
    for module in order.iter().copied().filter(|m| modelled.contains(m)).collect::<Vec<_>>() {
      let specs = public_modules.iter().find(|(m, _)| *m == module).map(|(_, s)| *s);
      let exports: Vec<(ArcStr, SymbolRef)> = self.sorted_exports(module);
      for (name, symbol) in exports {
        let spec =
          specs.and_then(|specs| specs.iter().find(|(n, _)| *n == name.as_str()).map(|(_, s)| *s));
        let chain = self.alias_chain(symbol);
        // Only declarations inside modelled modules take a model object: an export that aliases
        // ordinary code is that code (`export const x = internalRecord.x`).
        let generic_target = chain.iter().copied().find(|sym| !modelled.contains(&sym.owner));
        let chain: Vec<SymbolRef> =
          chain.into_iter().take_while(|sym| modelled.contains(&sym.owner)).collect();
        let (obj, rank) = match spec {
          Some(spec) => (effect::spec_object(&mut a, spec), BindRank::Rule),
          None => match chain.iter().find_map(|sym| self.key_declaration_of(*sym)) {
            Some((key_name, reference)) => {
              (effect::declared_key(&mut a, key_name.as_deref(), reference), BindRank::Key)
            }
            None => {
              if let Some(target) = generic_target {
                links.extend(chain.iter().map(|sym| (target, *sym)));
                continue;
              }
              (effect::default_object(&mut a), BindRank::Default)
            }
          },
        };
        // Precedence does not depend on the walk order: an explicit rule beats a Key, and a
        // Key beats the default. `internal/effect.js` is walked before `Effect.js`, and its
        // own `map` export must not keep the default once `Effect.map`'s rule reaches the
        // same declaration. At equal rank the export's own declaration takes its spec, and
        // the declarations it aliases keep what claimed them first (`Reference` aliases
        // `Service` in `Context.js`, and `Service` must keep its own rule).
        for (i, sym) in chain.into_iter().enumerate() {
          let claim = match bind_rank.get(&sym) {
            None => true,
            Some(held) if i == 0 => *held <= rank,
            Some(held) => *held < rank,
          };
          if claim {
            sh.rule_symbols.insert(sym, obj);
            bind_rank.insert(sym, rank);
          }
        }
      }
    }
    // The listed exports of otherwise ordinary modules (`effect::LISTED_MODULES`): their
    // declarations take the rule, and the generator skips them.
    for (module, specs) in &listed_modules {
      for (name, spec) in *specs {
        let Some(export) = self.metas[*module].resolved_exports.get(*name) else { continue };
        let obj = effect::spec_object(&mut a, *spec);
        for sym in self.alias_chain(export.symbol_ref) {
          if sym.owner == *module {
            sh.rule_symbols.entry(sym).or_insert(obj);
          }
        }
      }
    }
    for (target, sym) in links {
      if sh.rule_symbols.contains_key(&sym) {
        continue;
      }
      lazy_roots.push(target);
      let to = sh.symbol_var(&mut a, target);
      let from = sh.symbol_var(&mut a, sym);
      a.copy(to, from);
    }

    // `Effect.repeat`, `Effect.retry` and `Effect.retryOrElse` step a Schedule through the
    // bundled stable `Schedule.toStep`.
    if let Some(module) = schedule_module {
      if let Some(export) = self.metas[module].resolved_exports.get(effect::SCHEDULE_TO_STEP) {
        lazy_roots.push(export.symbol_ref);
        a.schedule_to_step = Some(sh.symbol_var(&mut a, export.symbol_ref));
      }
    }

    phase!("model bound, {} rule symbols", sh.rule_symbols.len());
    if debug {
      for (i, m) in self.module_table.modules.iter().enumerate() {
        eprintln!("[effect-service-shaking] module {i} = {}", m.id());
      }
    }
    // == per-module facts and constraint generation
    let mut module_fns = vec![];
    for module in order.iter().map(|idx| &self.module_table.modules[*idx]) {
      let Module::Normal(normal) = module else { continue };
      let Some(ast) = self.ast_table[normal.idx].as_ref() else { continue };
      // Modelled modules sit behind rules; their bodies are never analysed.
      if modelled.contains(&normal.idx) {
        continue;
      }
      let scoping = self.symbols[normal.idx].as_ref().expect("module symbols").scoping();
      let mut external_imports = FxHashMap::default();
      let mut cjs_imports = FxHashSet::default();
      for (sym, import) in &normal.ecma_view.named_imports {
        let record = &normal.ecma_view.import_records[import.record_idx];
        match record.resolved_module {
          Some(target) => match &self.module_table.modules[target] {
            Module::External(_) => {
              let obj = sh.externals[&target];
              let name = match &import.imported {
                Specifier::Star => None,
                Specifier::Literal(l) => Some(ArcStr::from(l.as_str())),
              };
              external_imports.insert(*sym, (obj, name));
            }
            Module::Normal(_) => {
              if sh.cjs_modules.contains(&target) {
                cjs_imports.insert(*sym);
              }
            }
          },
          None => {
            let obj = host::external_module(&mut a, record.module_request.as_str());
            let name = match &import.imported {
              Specifier::Star => None,
              Specifier::Literal(l) => Some(ArcStr::from(l.as_str())),
            };
            external_imports.insert(*sym, (obj, name));
          }
        }
      }
      let mut dynamic_imports = FxHashMap::default();
      for (node_id, record_id) in &normal.ecma_view.imports {
        let record = &normal.ecma_view.import_records[*record_id];
        let target = record
          .resolved_module
          .filter(|t| matches!(self.module_table.modules[*t], Module::Normal(_)));
        dynamic_imports.insert(node_id.index(), target);
      }
      let facts = ModuleFacts {
        module: normal.idx,
        scoping,
        external_imports,
        cjs_imports,
        dynamic_imports,
        default_export: normal.ecma_view.default_export_ref,
        lazy_stmts: self.lazy_statements(normal.idx, ast.program()),
      };
      let mut g = Gen::new(&mut a, &mut sh, &facts);
      g.program(ast.program());
      module_fns.push(g.module_fn());
      phase!("generated {} ({} vars, {} objs)", normal.id.as_ref(), a.vars.len(), a.objs.len());
    }

    // == namespace objects hold every export
    for module in order.iter().map(|idx| &self.module_table.modules[*idx]) {
      let Module::Normal(normal) = module else { continue };
      let ns = sh.namespace_objs[&normal.idx];
      let exports: Vec<(ArcStr, SymbolRef)> = self.sorted_exports(normal.idx);
      for (name, symbol) in exports {
        let v = sh.symbol_var(&mut a, symbol);
        let f = a.field(ns, &name);
        a.copy(v, f);
      }
      if sh.cjs_modules.contains(&normal.idx) {
        let elem = a.elem(ns);
        a.seed(elem, a.top_ext);
      }
    }

    // == entry exports escape to the consumer
    for idx in self.user_defined_entry_modules.iter().copied().collect::<Vec<_>>() {
      let Module::Normal(normal) = &self.module_table.modules[idx] else { continue };
      let exports: Vec<(ArcStr, SymbolRef)> = self.sorted_exports(idx);
      for (_, symbol) in exports {
        lazy_roots.push(symbol);
        let v = sh.symbol_var(&mut a, symbol);
        a.constrain(v, Constraint::Escape(Flavour::Ext, None));
      }
      if sh.cjs_modules.contains(&idx) || self.metas[idx].has_dynamic_exports {
        let ns = sh.namespace_objs[&normal.idx];
        let v = a.var();
        a.seed(v, ns);
        a.constrain(v, Constraint::Escape(Flavour::Ext, None));
      }
    }

    // == lazy top-level statements: a reference activates the statement declaring the symbol
    // (design.md, principle 2). Roots are the references made outside generated code.
    {
      let mut seen: FxHashSet<(FnId, FnId)> = FxHashSet::default();
      for (owner, sym) in std::mem::take(&mut sh.lazy_refs) {
        if let Some(l) = sh.lazy_decls.get(&sym).copied() {
          if l != owner && seen.insert((owner, l)) {
            a.fns[owner].pending.push(Pending::Activate(l));
          }
        }
      }
      for module in order.iter().copied() {
        let Some(ns) = sh.namespace_objs.get(&module).copied() else { continue };
        let mut all: Vec<FnId> = sh.lazy_by_module.get(&module).cloned().unwrap_or_default();
        for (name, symbol) in self.sorted_exports(module) {
          let canonical = self.symbols.canonical_ref_for(symbol);
          if let Some(l) = sh.lazy_decls.get(&canonical).copied() {
            a.lazy_exports.insert((ns, name), l);
            all.push(l);
          }
        }
        if !all.is_empty() {
          a.lazy_namespaces.insert(ns, all);
        }
      }
      for sym in lazy_roots {
        if let Some(l) = sh.lazy_decls.get(&self.symbols.canonical_ref_for(sym)).copied() {
          a.activate(l);
        }
      }
    }

    // == solve
    for f in module_fns {
      a.activate(f);
    }
    phase!("solving");
    a.solve();

    let solved = start.elapsed();
    phase!("solved: {}", a.histogram());
    if debug {
      let unknown: Vec<String> = a
        .pts(a.unknown_key_values)
        .iter()
        .map(|o| {
          format!(
            "{} from {:?} demand={:?} escaped={:?}",
            a.objs[*o].kind.clone().pipe_kind(),
            a.objs[*o].site,
            a.objs[*o].demand,
            a.objs[*o].escaped
          )
        })
        .collect();
      eprintln!("[effect-service-shaking] provided through unknown keys: {unknown:#?}");
      for (id, o) in a.objs.iter_enumerated() {
        if let ObjKind::Class { heritage: Some(h), .. } = o.kind {
          let kinds: Vec<String> =
            a.pts(h).iter().map(|x| a.objs[*x].kind.clone().pipe_kind()).collect();
          eprintln!("[effect-service-shaking] class {id:?} from {:?} extends {kinds:?}", o.site);
        }
        if let ObjKind::Key { strings, reference } = o.kind {
          let names: Vec<String> = a
            .pts(strings)
            .iter()
            .map(|s| format!("{:?}", a.objs[*s].kind).chars().take(60).collect())
            .collect();
          eprintln!(
            "[effect-service-shaking] key object {id:?} reference={reference} from {:?}: {names:?}",
            o.site
          );
        }
      }
    }

    // == prune
    let names: Vec<String> = self.module_table.modules.iter().map(|m| m.id().to_string()).collect();
    let (edits, mut report) =
      prune::plan(&mut a, &sh, &|idx: ModuleIdx| names[idx.index()].clone());
    report.bail_reasons.clone_from(&a.bail_reasons);
    phase!("planned {} removals", report.removed.len());
    drop(sh);
    for (module, properties) in &edits.properties {
      let scoping = self.symbols[*module].as_ref().expect("module symbols").scoping();
      if let Some(ast) = self.ast_table[*module].as_mut() {
        prune::apply(ast, scoping, &mut self.stmt_infos[*module], *module, properties);
      }
    }

    let total_removed = report.removed.len();
    tracing::debug!(
      vars = a.vars.len(),
      objs = a.objs.len(),
      steps = a.steps,
      removed = total_removed,
      solve_ms = solved.as_millis(),
      total_ms = start.elapsed().as_millis(),
      "effect service shaking"
    );
    #[expect(clippy::print_stderr, reason = "opt-in report for measuring the experimental pass")]
    if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT").is_some() {
      eprintln!(
        "[effect-service-shaking] {} vars, {} objects, {} steps, solve {} ms, total {} ms",
        a.vars.len(),
        a.objs.len(),
        a.steps,
        solved.as_millis(),
        start.elapsed().as_millis()
      );
      if report.bail_all {
        eprintln!("[effect-service-shaking] every key kept:");
      }
      for r in &report.bail_reasons {
        eprintln!("[effect-service-shaking]   {r}");
      }
      for (i, name) in names.iter().enumerate() {
        if report
          .bail_reasons
          .iter()
          .any(|r| r.contains(&format!("module: {i},")) || r.contains(&format!("module {i} ")))
        {
          eprintln!("[effect-service-shaking]   module {i} = {name}");
        }
      }
      for k in &report.keys {
        eprintln!(
          "[effect-service-shaking] key {} providers={} values={} removed={} kept={}{} demand={}",
          k.key,
          k.providers,
          k.values,
          k.removed,
          k.kept,
          if k.bailed { " (bailed)" } else { "" },
          k.demand
        );
      }
      let mut removed = report.removed;
      removed.sort();
      for (module, start, name) in removed {
        eprintln!("[effect-service-shaking] removed {name} at {module}:{start}");
      }
    }
  }

  /// A module's exports in name order: `resolved_exports` is a hash map whose iteration order
  /// follows its insertion history, and the walks over it allocate variables.
  fn sorted_exports(&self, module: ModuleIdx) -> Vec<(ArcStr, SymbolRef)> {
    let mut exports: Vec<(ArcStr, SymbolRef)> = self.metas[module]
      .resolved_exports
      .iter()
      .map(|(n, e)| (ArcStr::from(n.as_str()), e.symbol_ref))
      .collect();
    exports.sort_unstable_by(|x, y| x.0.cmp(&y.0));
    exports
  }

  /// The top-level statements of `module` whose constraints can wait until something
  /// references what they declare: those Rolldown finds free of side effects that declare
  /// symbols of their own (imports and re-exports name other modules' declarations). Entry
  /// modules run eagerly. By body index, with the declared symbols.
  fn lazy_statements(
    &self,
    module: ModuleIdx,
    program: &oxc::ast::ast::Program<'_>,
  ) -> FxHashMap<usize, Vec<SymbolRef>> {
    let mut lazy = FxHashMap::default();
    if self.user_defined_entry_modules.contains(&module) {
      return lazy;
    }
    let infos = &self.stmt_infos[module];
    for (i, stmt) in program.body.iter().enumerate() {
      let idx = rolldown_common::StmtInfoIdx::new(i + 1);
      if idx.index() >= infos.infos.len() {
        break;
      }
      let info = infos.get(idx);
      let reexport = matches!(
        stmt,
        Statement::ImportDeclaration(_)
          | Statement::ExportAllDeclaration(_)
          | Statement::ExportFromDeclaration(_)
      );
      if reexport
        || info.eval_flags.has_side_effect_for_tree_shaking()
        || info.declared_symbols.is_empty()
      {
        continue;
      }
      let declared: Vec<SymbolRef> =
        info.declared_symbols.iter().map(rolldown_common::TaggedSymbolRef::inner).collect();
      if declared.iter().all(|s| self.symbols.canonical_ref_for(*s) == *s) {
        lazy.insert(i, declared);
      }
    }
    lazy
  }

  /// A public export and the declarations it aliases: `export const map = internal.map`
  /// binds the rule to `internal.map`'s declaration too, so internal callers hit it.
  fn alias_chain(&self, symbol: SymbolRef) -> Vec<SymbolRef> {
    let mut chain = vec![];
    let mut current = self.symbols.canonical_ref_for(symbol);
    for _ in 0..8 {
      if chain.contains(&current) {
        break;
      }
      chain.push(current);
      let Some(next) = self.alias_target(current) else { break };
      current = self.symbols.canonical_ref_for(next);
    }
    chain
  }

  /// Whether the symbol's top-level declaration creates a service key.
  fn key_declaration_of(&self, symbol: SymbolRef) -> Option<(Option<String>, bool)> {
    let module = symbol.owner;
    let ast = self.ast_table[module].as_ref()?;
    let program = ast.program();
    if let Some(init) = find_top_level_init(program, symbol.symbol) {
      return effect::key_declaration(init);
    }
    // `class K extends Context.Service<..>()("key") {}`
    for stmt in &program.body {
      let class = match stmt {
        Statement::ClassDeclaration(c) => c,
        Statement::ExportDeclaration(e) => match &e.declaration {
          oxc::ast::ast::Declaration::ClassDeclaration(c) => c,
          _ => continue,
        },
        Statement::ExportDefaultDeclaration(d) => match &d.declaration {
          oxc::ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(c) => c,
          _ => continue,
        },
        _ => continue,
      };
      if class.id.as_ref().and_then(|id| id.symbol_id.get()) == Some(symbol.symbol) {
        let heritage = class.heritage.as_ref()?;
        let _: &[ClassElement<'_>] = &class.body.body;
        return effect::key_declaration(&heritage.expression);
      }
    }
    None
  }

  /// The symbol a top-level `const x = y` or `const x = ns.y` declaration aliases.
  fn alias_target(&self, symbol: SymbolRef) -> Option<SymbolRef> {
    let module = symbol.owner;
    let Module::Normal(normal) = &self.module_table.modules[module] else { return None };
    let ast = self.ast_table[module].as_ref()?;
    let scoping = self.symbols[module].as_ref()?.scoping();
    let init = find_top_level_init(ast.program(), symbol.symbol)?;
    match init {
      Expression::Identifier(id) => {
        let reference = id.reference_id.get()?;
        let target = scoping.get_reference(reference).symbol_id()?;
        Some((module, target).into())
      }
      Expression::StaticMemberExpression(member) => {
        let Expression::Identifier(object) = &member.object else { return None };
        let reference = object.reference_id.get()?;
        let target = scoping.get_reference(reference).symbol_id()?;
        let ns_symbol = self.symbols.canonical_ref_for((module, target).into());
        // A namespace import: find the module whose namespace symbol this is.
        let ns_module = self.module_table.modules.iter().find_map(|m| match m {
          Module::Normal(n)
            if self.symbols.canonical_ref_for(n.ecma_view.namespace_object_ref) == ns_symbol =>
          {
            Some(n.idx)
          }
          _ => None,
        })?;
        let _ = normal;
        self.metas[ns_module]
          .resolved_exports
          .get(member.property.name.as_str())
          .map(|e| e.symbol_ref)
      }
      _ => None,
    }
  }
}

/// What a model-bound declaration is bound to, by precedence: an explicit stable rule wins
/// over a declared Key, which wins over `effect:default`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BindRank {
  Default,
  Key,
  Rule,
}

trait PipeKind {
  fn pipe_kind(self) -> String;
}
impl PipeKind for ObjKind {
  fn pipe_kind(self) -> String {
    format!("{self:?}").split([' ', '(', '{']).next().unwrap_or("").to_string()
  }
}

fn find_top_level_init<'a>(
  program: &'a oxc::ast::ast::Program<'a>,
  symbol: oxc::semantic::SymbolId,
) -> Option<&'a Expression<'a>> {
  for stmt in &program.body {
    let decl = match stmt {
      Statement::VariableDeclaration(d) => d,
      Statement::ExportDeclaration(e) => match &e.declaration {
        oxc::ast::ast::Declaration::VariableDeclaration(d) => d,
        _ => continue,
      },
      _ => continue,
    };
    for declarator in &decl.declarations {
      if let oxc::ast::ast::BindingPattern::BindingIdentifier(id) = &declarator.id {
        if id.symbol_id.get() == Some(symbol) {
          return declarator.init.as_ref();
        }
      }
    }
  }
  None
}
