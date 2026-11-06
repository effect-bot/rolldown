//! Removal of Effect service methods that nothing in the bundle can read
//! (`experimental.effectServiceShaking`, EFF-3430).
//!
//! A whole-bundle, inclusion-based value-flow analysis (`heap.rs`, `gen.rs`)
//! with built-in models of JavaScript builtins (`host.rs`) and of the stable
//! primitives of `effect` 4.x (`effect.rs`). Demand is recorded per key and
//! applied to every provider of that key; `prune.rs` turns it into property
//! removals and patches the statement infos so ordinary tree shaking removes
//! what the removed methods were the only users of.
//!
//! Runs after `cross_module_optimization` and before `include_statements`.

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
use rustc_hash::{FxHashMap, FxHashSet};

use self::generate::{Gen, ModuleFacts, Shared};
use self::heap::{Analysis, Constraint, Flavour, ObjKind};
use super::LinkStage;

impl LinkStage<'_> {
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
      resolve_global: host::resolve_global,
    };

    // == modules: namespaces, externals, CommonJS
    for module in &self.module_table.modules {
      match module {
        Module::Normal(normal) => {
          let ns = a.alloc(ObjKind::Namespace(normal.idx), None);
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
    let mut effect_found = false;
    let mut effect_external = false;
    for module in &self.module_table.modules {
      match module {
        Module::Normal(normal) => {
          let Some(pkg) = normal.originative_resolved_id.package_json.as_ref() else { continue };
          if pkg.name() != Some("effect") {
            continue;
          }
          effect_found = true;
          let path = normal.id.as_ref().replace('\\', "/");
          if effect::GENERIC_MODULES.iter().any(|suffix| path.ends_with(suffix)) {
            continue;
          }
          let public =
            effect::MODULES.iter().find(|(name, _)| path.ends_with(&format!("/dist/{name}.js")));
          let data = effect::DATA_MODULES.iter().any(|suffix| path.ends_with(suffix));
          let exports: Vec<(ArcStr, SymbolRef)> = self.metas[normal.idx]
            .resolved_exports
            .iter()
            .map(|(n, e)| (ArcStr::from(n.as_str()), e.symbol_ref))
            .collect();
          for (name, symbol) in exports {
            let spec = public.and_then(|(_, specs)| {
              specs.iter().find(|(n, _)| *n == name.as_str()).map(|(_, s)| *s)
            });
            let chain = self.alias_chain(symbol);
            let obj = match spec {
              Some(spec) => effect::spec_object(&mut a, spec),
              None => match chain.iter().find_map(|sym| self.key_declaration_of(*sym)) {
                Some((key_name, reference)) => {
                  effect::declared_key(&mut a, key_name.as_deref(), reference)
                }
                None if data => effect::data_object(&mut a),
                None => effect::default_object(&mut a),
              },
            };
            // The export's own declaration always gets its spec; the declarations it aliases
            // take it only when nothing more specific claimed them (`Reference` aliases
            // `Service` in `Context.js`, and `Service` must keep its own rule).
            let mut chain = chain.into_iter();
            if let Some(own) = chain.next() {
              sh.rule_symbols.insert(own, obj);
            }
            for sym in chain {
              sh.rule_symbols.entry(sym).or_insert(obj);
            }
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
    if effect_external && !effect_found {
      tracing::debug!("effect service shaking: `effect` is external, nothing can be recognised");
    }

    phase!("model bound, {} rule symbols", sh.rule_symbols.len());
    if debug {
      for (i, m) in self.module_table.modules.iter().enumerate() {
        eprintln!("[effect-service-shaking] module {i} = {}", m.id());
      }
    }
    // == per-module facts and constraint generation
    let mut module_fns = vec![];
    for module in &self.module_table.modules {
      let Module::Normal(normal) = module else { continue };
      let Some(ast) = self.ast_table[normal.idx].as_ref() else { continue };
      if normal
        .originative_resolved_id
        .package_json
        .as_ref()
        .is_some_and(|p| p.name() == Some("effect"))
      {
        let path = normal.id.as_ref().replace('\\', "/");
        if !effect::GENERIC_MODULES.iter().any(|suffix| path.ends_with(suffix)) {
          continue;
        }
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
      };
      let mut g = Gen::new(&mut a, &mut sh, &facts);
      g.program(ast.program());
      module_fns.push(g.module_fn());
      phase!("generated {} ({} vars, {} objs)", normal.id.as_ref(), a.vars.len(), a.objs.len());
    }

    // == namespace objects hold every export
    for module in &self.module_table.modules {
      let Module::Normal(normal) = module else { continue };
      let ns = sh.namespace_objs[&normal.idx];
      let exports: Vec<(ArcStr, SymbolRef)> = self.metas[normal.idx]
        .resolved_exports
        .iter()
        .map(|(n, e)| (ArcStr::from(n.as_str()), e.symbol_ref))
        .collect();
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
      let exports: Vec<SymbolRef> =
        self.metas[idx].resolved_exports.values().map(|e| e.symbol_ref).collect();
      for symbol in exports {
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
