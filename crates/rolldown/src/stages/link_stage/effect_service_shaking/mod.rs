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

mod dts;
mod effect;
mod generate;
mod heap;
mod host;
mod prune;

use std::cell::{OnceCell, RefCell};

use arcstr::ArcStr;
use oxc::ast::ast::{ClassElement, Expression, Statement};
use oxc_index::IndexVec;
use rolldown_common::{ExportsKind, Module, ModuleIdx, Specifier, SymbolRef};
use rolldown_error::{BuildDiagnostic, EventKindSwitcher};
use rustc_hash::{FxHashMap, FxHashSet};

use self::generate::{Gen, ModuleFacts, Shared};
use self::heap::{Analysis, Constraint, Flavour, FnId, ObjKind, Pending};
use super::LinkStage;

/// The work of every solve of one build (`effect_service_shaking_with` runs one per round).
#[derive(Default)]
struct Rounds {
  count: usize,
  entries: u64,
  steps: u64,
  solve: std::time::Duration,
  /// The `.d.ts` files the previous solve parsed. They depend on nothing but the files, so the
  /// next solve starts with them instead of parsing them again.
  dts: Option<dts::Dts>,
  /// Each solve's own entries and steps, for the report.
  per_solve: Vec<(u64, u64)>,
  /// The declarations the link stage started loading, for the first solve.
  declarations: Option<EffectDeclarations>,
  phases: Phases,
}

/// Whether a module is one of the `effect` package's.
fn is_effect(module: &Module) -> bool {
  match module {
    Module::Normal(normal) => normal
      .originative_resolved_id
      .package_json
      .as_ref()
      .is_some_and(|pkg| pkg.name() == Some("effect")),
    Module::External(_) => false,
  }
}

/// The `.d.ts` beside a module (`dist/Effect.js` has `dist/Effect.d.ts`).
fn declarations_of(module: &Module) -> Option<std::path::PathBuf> {
  module.id().as_str().strip_suffix(".js").map(|js| std::path::PathBuf::from(format!("{js}.d.ts")))
}

/// The declarations of `effect`'s modules, read and parsed on the rayon pool while the link
/// stage runs up to the pass (`LinkStage::preload_effect_declarations`).
pub(super) struct EffectDeclarations {
  /// Each module of `effect`, with the `.d.ts` beside it if its id names one.
  modules: Vec<(ModuleIdx, Option<std::path::PathBuf>)>,
  loading: Loading,
}

enum Loading {
  #[cfg(not(target_family = "wasm"))]
  Started(Preload),
  /// Not started: loaded when the pass needs them.
  Deferred,
}

/// A load running on the rayon pool. It belongs to the build that started it: whatever way the
/// pass ends, the load has ended before the pass returns (`Drop`).
#[cfg(not(target_family = "wasm"))]
struct Preload {
  /// Set when the pass ends without the files: the load stops starting new ones.
  cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
  /// Taken once the result is received.
  receiver: Option<std::sync::mpsc::Receiver<std::thread::Result<dts::Loaded>>>,
}

#[cfg(not(target_family = "wasm"))]
impl Preload {
  fn start(
    paths: Vec<std::path::PathBuf>,
    load: impl FnOnce(&[std::path::PathBuf], &std::sync::atomic::AtomicBool) -> dts::Loaded
    + Send
    + 'static,
  ) -> Self {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::channel();
    let flag = std::sync::Arc::clone(&cancel);
    rayon::spawn(move || {
      let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(&paths, &flag)));
      // Only fails if the receiver is gone, which `Drop` rules out until this is sent.
      let _ = sender.send(loaded);
    });
    Self { cancel, receiver: Some(receiver) }
  }

  /// Waits for the files. `None` if the load ended without sending them.
  fn wait(&mut self) -> Option<dts::Loaded> {
    match self.receiver.take()?.recv() {
      Ok(Ok(loaded)) => Some(loaded),
      Ok(Err(panic)) => std::panic::resume_unwind(panic),
      Err(_) => None,
    }
  }
}

/// The pass may end without the files: a contract check that fails (`effect` external, an
/// unsupported version, missing exports) or a panic. The load must not outlive it, reading
/// files and holding parsed arenas into rendering or the next build, so this stops the files
/// not yet started and waits for those in flight. The pass is not on a rayon worker when a load
/// was started (`preload_effect_declarations`), so the wait blocks no worker.
#[cfg(not(target_family = "wasm"))]
impl Drop for Preload {
  fn drop(&mut self) {
    if let Some(receiver) = self.receiver.take() {
      self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
      let _ = receiver.recv();
    }
  }
}

fn paths_of(modules: &[(ModuleIdx, Option<std::path::PathBuf>)]) -> Vec<std::path::PathBuf> {
  modules.iter().filter_map(|(_, dts)| dts.clone()).collect()
}

fn load_now(paths: &[std::path::PathBuf]) -> dts::Loaded {
  dts::load(paths, &std::sync::atomic::AtomicBool::new(false))
}

impl EffectDeclarations {
  fn new(modules: &[&Module]) -> Self {
    Self {
      modules: modules.iter().map(|m| (m.idx(), declarations_of(m))).collect(),
      loading: Loading::Deferred,
    }
  }

  /// Hands the parsed files to `dts` in the order of `order` (the pass's execution order, as
  /// files are numbered as they are added) and returns what was found for each module's
  /// `.d.ts`. They were started in the module table's order, which varies between builds.
  fn adopt(self, dts: &dts::Dts, order: &[ModuleIdx]) -> FxHashMap<ModuleIdx, dts::Read> {
    let loaded = match self.loading {
      #[cfg(not(target_family = "wasm"))]
      Loading::Started(mut preload) => {
        preload.wait().unwrap_or_else(|| load_now(&paths_of(&self.modules)))
      }
      Loading::Deferred => load_now(&paths_of(&self.modules)),
    };
    let position: FxHashMap<ModuleIdx, usize> = self
      .modules
      .iter()
      .filter(|(_, d)| d.is_some())
      .enumerate()
      .map(|(i, (m, _))| (*m, i))
      .collect();
    let ordered: Vec<ModuleIdx> =
      order.iter().copied().filter(|m| position.contains_key(m)).collect();
    let loaded = loaded.ordered(ordered.iter().map(|m| position[m]));
    ordered.into_iter().zip(dts.adopt(loaded)).collect()
  }
}

/// The phases of one solve, as the report's `phases` line names them.
const PHASES: [&str; 8] =
  ["declarations", "bind", "generate", "solve", "after solve", "plan", "apply", "drop"];

/// Wall time spent in each of `PHASES`, summed over the solves of one build. Measured in every
/// build (a clock read per phase), so the report's numbers do not depend on the debug output.
#[derive(Default)]
struct Phases {
  at: Option<std::time::Instant>,
  spent: [std::time::Duration; PHASES.len()],
}

impl Phases {
  fn start(&mut self) {
    self.at = Some(std::time::Instant::now());
  }

  /// Ends phase `i` (an index into `PHASES`) now; the next phase starts here.
  fn lap(&mut self, i: usize) {
    let now = std::time::Instant::now();
    if let Some(at) = self.at {
      self.spent[i] += now - at;
    }
    self.at = Some(now);
  }

  fn report(&self) -> String {
    let parts: Vec<String> = PHASES
      .iter()
      .zip(self.spent)
      .map(|(name, d)| format!("{name} {:.1}", d.as_secs_f64() * 1000.0))
      .collect();
    format!("{} ms", parts.join(", "))
  }
}

impl LinkStage<'_> {
  /// Starts reading and parsing the declarations of `effect`'s modules on the rayon pool, so
  /// that the link stage's work up to the pass hides it. They depend only on the module table.
  /// A link stage that runs on a rayon worker itself loads them when the pass needs them: it
  /// would block that worker waiting for them.
  pub(super) fn preload_effect_declarations(&self) -> Option<EffectDeclarations> {
    if !self.options.experimental.is_effect_service_shaking_enabled() {
      return None;
    }
    let modules: Vec<&Module> = self.module_table.modules.iter().filter(|m| is_effect(m)).collect();
    if modules.is_empty() {
      return None;
    }
    let declarations = EffectDeclarations::new(&modules);
    #[cfg(not(target_family = "wasm"))]
    let declarations = if rayon::current_thread_index().is_none() {
      let preload = Preload::start(paths_of(&declarations.modules), dts::load);
      EffectDeclarations { loading: Loading::Started(preload), ..declarations }
    } else {
      declarations
    };
    Some(declarations)
  }

  fn warn_effect_service_shaking(&mut self, reason: String) {
    if self.options.checks.contains(EventKindSwitcher::EffectServiceShaking) {
      self
        .diagnostics
        .push(BuildDiagnostic::effect_service_shaking(reason).with_severity_warning());
    }
  }

  #[tracing::instrument(level = "debug", skip_all)]
  pub(super) fn effect_service_shaking(&mut self, declarations: Option<EffectDeclarations>) {
    if !self.options.experimental.is_effect_service_shaking_enabled() {
      return;
    }
    let start = std::time::Instant::now();
    // Each solve that finds a service, a key or a provider in a module hole analyses those
    // modules in the next one (`module_holes`). The set only grows, so this ends.
    let mut analysed_holes: FxHashSet<ModuleIdx> = FxHashSet::default();
    let mut rounds = Rounds { declarations, ..Rounds::default() };
    while self.effect_service_shaking_with(start, &mut analysed_holes, &mut rounds) {}
  }

  /// One solve with the module holes `module_holes` leaves after `analysed_holes`. Returns
  /// true, without pruning, when a hole has to be analysed: it is then in `analysed_holes`.
  #[expect(clippy::too_many_lines, reason = "one orchestration: bind, generate, solve, prune")]
  fn effect_service_shaking_with(
    &mut self,
    start: std::time::Instant,
    analysed_holes: &mut FxHashSet<ModuleIdx>,
    rounds: &mut Rounds,
  ) -> bool {
    let debug = std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_DEBUG").is_some();
    let report_enabled = std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT").is_some();
    macro_rules! phase {
      ($($t:tt)*) => {
        if debug {
          eprintln!("[effect-service-shaking] {} ms: {}", start.elapsed().as_millis(), format!($($t)*));
        }
      };
    }
    rounds.phases.start();
    let mut a = Analysis::new();
    let decls = Decls::default();
    if let Some(limit) = self.options.experimental.effect_service_shaking_widen {
      a.widen_limit = limit as usize;
    }
    if let Some(max) = self.options.experimental.effect_service_shaking_max_entries {
      a.max_entries = max;
    }
    if let Some(dts) = rounds.dts.take() {
      a.dts = dts;
    }
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
      key_symbols: FxHashSet::default(),
      key_ctor_symbols: FxHashSet::default(),
      key_ctor_members: FxHashSet::default(),
      hole_values: FxHashMap::default(),
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
    // type-modelled from its `.d.ts`, except the service-constructor modules, which are analysed
    // as ordinary code (below). See design sections 3 and 11.
    let mut effect_found = false;
    let mut effect_external = false;
    let mut unsupported_version: Option<String> = None;
    let mut public_modules: Vec<(ModuleIdx, &'static [(&'static str, effect::Spec)])> = vec![];
    let mut listed_modules: Vec<(ModuleIdx, &'static [(&'static str, effect::Spec)])> = vec![];
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
      return false;
    }
    if !effect_found {
      // No `effect` in the bundle: no service key can exist.
      return false;
    }
    if let Some(version) = unsupported_version {
      self.warn_effect_service_shaking(format!(
        "`effect` {version} is not a supported version (the model covers {}.x)",
        effect::SUPPORTED_MAJOR
      ));
      return false;
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
      return false;
    }
    // == declarations: `effect`'s modules are modelled from the `.d.ts` next to each (design
    // principle 11). Without them there is nothing to model `effect` by, so one warning says
    // which declaration is missing or unreadable and every method is kept. Nothing else reads
    // `effect`'s bodies instead.
    let is_effect_module = |idx: ModuleIdx| is_effect(&self.module_table.modules[idx]);
    let declarations = |idx: ModuleIdx| declarations_of(&self.module_table.modules[idx]);
    // They are read and parsed in parallel, from the start of the link stage, since the model
    // reads most of them (`preload_effect_declarations`). A later solve has the same modules,
    // whose declarations the first one found readable and parsed.
    let mut reads = if rounds.count == 0 {
      let effect_modules: Vec<&Module> =
        order.iter().map(|m| &self.module_table.modules[*m]).filter(|m| is_effect(m)).collect();
      let started = rounds.declarations.take();
      started.unwrap_or_else(|| EffectDeclarations::new(&effect_modules)).adopt(&a.dts, &order)
    } else {
      FxHashMap::default()
    };
    for module in order.iter().copied().filter(|m| rounds.count == 0 && is_effect_module(*m)) {
      let id = self.module_table[module].id().as_str();
      let problem = match (declarations(module), reads.remove(&module)) {
        (None, _) => Some(format!("`{id}` has no `.d.ts` declarations")),
        (Some(dts), Some(dts::Read::Missing)) => Some(format!("`{}` is missing", dts.display())),
        (Some(dts), Some(dts::Read::Unreadable(err))) => {
          Some(format!("`{}` can't be read ({err})", dts.display()))
        }
        (Some(_), _) => None,
      };
      if let Some(problem) = problem {
        self.warn_effect_service_shaking(format!(
          "`effect`'s declarations are needed to model it, and {problem}; every method is kept"
        ));
        return false;
      }
    }
    rounds.phases.lap(0);
    let mut modelled: FxHashSet<ModuleIdx> = public_modules.iter().map(|(m, _)| *m).collect();
    for (module, specs) in &public_modules {
      for (name, export) in &self.metas[*module].resolved_exports {
        if specs.iter().any(|(n, _)| *n == name.as_str()) {
          for sym in self.alias_chain(&decls, export.symbol_ref) {
            modelled.insert(sym.owner);
          }
        }
      }
    }
    // == type-modelled library modules (design section 11): every other module of the `effect`
    // package, the listed modules' unlisted exports included, is modelled from its `.d.ts`
    // instead of its body. Its exports are `ObjKind::Typed` values; its body is not generated.
    let mut typed_modules: FxHashSet<ModuleIdx> = FxHashSet::default();
    // Service constructors (Tim's decision on NodeFileSystem, option (b)): exports whose `.d.ts`
    // takes an implementation and returns the shape of a key declared in `effect`, or an Effect
    // or Layer of it, found by type (`Dts::constructed_shapes`). Their modules are analysed as
    // ordinary code, so the default methods they build count as reads, together with the
    // internal modules they import (helpers with no public types to summarise, such as
    // `internal/glob.js` behind `FileSystem.make`). Everything else they reach is type-modelled.
    let mut constructors: Vec<(String, ArcStr, String)> = vec![];
    let mut constructor_keys: Vec<(ArcStr, ModuleIdx)> = vec![];
    let mut analysed: FxHashSet<ModuleIdx> = FxHashSet::default();
    for module in order.iter().copied().filter(|m| is_effect_module(*m) && !modelled.contains(m)) {
      let dts = declarations(module).expect("checked with the declarations");
      for (name, _) in self.sorted_exports(module) {
        for (iface, local) in a.dts.constructed_shapes(&dts, name.as_str()) {
          let Some(target) = iface
            .to_str()
            .and_then(|p| p.strip_suffix(".d.ts"))
            .map(|p| format!("{p}.js"))
            .and_then(|p| order.iter().copied().find(|m| self.module_table[*m].id().as_str() == p))
          else {
            continue;
          };
          let Some(export) = self.metas[target].resolved_exports.get(local.as_str()) else {
            continue;
          };
          let key = self
            .alias_chain(&decls, export.symbol_ref)
            .into_iter()
            .find_map(|sym| self.key_declaration_of(&decls, sym).map(|k| (k, sym.owner)));
          let Some(((Some(key_name), _), owner)) = key else { continue };
          if !is_effect_module(owner) {
            continue;
          }
          let file = self.module_table[module].id().as_str();
          let file = file.rsplit_once("/dist/").map_or(file, |(_, f)| f).to_string();
          // Diagnostic only: leave matching constructor modules type-modelled.
          if std::env::var("ROLLDOWN_EFFECT_SERVICE_SHAKING_SKIP_CONSTRUCTOR")
            .is_ok_and(|v| !v.is_empty() && file.contains(v.as_str()))
          {
            continue;
          }
          constructors.push((file, name.clone(), key_name.clone()));
          if !constructor_keys.iter().any(|(k, _)| k.as_str() == key_name) {
            constructor_keys.push((ArcStr::from(key_name.as_str()), owner));
          }
          analysed.insert(module);
          analysed.insert(owner);
        }
      }
    }
    let helpers: Vec<ModuleIdx> = analysed
      .iter()
      .filter_map(|m| match &self.module_table.modules[*m] {
        Module::Normal(n) => Some(n),
        Module::External(_) => None,
      })
      .flat_map(|n| n.ecma_view.import_records.iter().filter_map(|r| r.resolved_module))
      .filter(|t| {
        is_effect_module(*t)
          && !modelled.contains(t)
          && self.module_table[*t].id().as_str().contains("/internal/")
      })
      .collect();
    analysed.extend(helpers);
    for module in order.iter().copied() {
      if is_effect_module(module) && !modelled.contains(&module) && !analysed.contains(&module) {
        typed_modules.insert(module);
        modelled.insert(module);
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
        let chain = self.alias_chain(&decls, symbol);
        // Only declarations inside modelled modules take a model object: an export that aliases
        // ordinary code is that code (`export const x = internalRecord.x`).
        let generic_target = chain.iter().copied().find(|sym| !modelled.contains(&sym.owner));
        let chain: Vec<SymbolRef> =
          chain.into_iter().take_while(|sym| modelled.contains(&sym.owner)).collect();
        let (obj, rank) = match spec {
          Some(spec) => (effect::spec_object(&mut a, spec), BindRank::Rule),
          None => {
            match chain
              .iter()
              .find_map(|sym| self.key_declaration_of(&decls, *sym).map(|k| (k, sym.owner)))
            {
              Some(((key_name, reference), owner)) => {
                // A key declared inside type-modelled `effect` code: that code uses its service
                // in ways the pass can't see, so the key keeps every method.
                if typed_modules.contains(&owner) {
                  if let Some(n) = &key_name {
                    a.never_prune.insert(ArcStr::from(n.as_str()));
                  }
                }
                (effect::declared_key(&mut a, key_name.as_deref(), reference), BindRank::Key)
              }
              None => {
                if let Some(target) = generic_target {
                  links.extend(chain.iter().map(|sym| (target, *sym)));
                  continue;
                }
                if typed_modules.contains(&module) {
                  // A constant key (`Equal.symbol`, `Redactable.symbolRedactable`) is that key:
                  // a summary of it would make every computed member written with it a store
                  // under an unknown name.
                  let obj = match self.constant_key_through(&chain) {
                    Some(key) => a.str(&key),
                    None => {
                      let dts = declarations(module).expect("checked with the declarations");
                      a.typed_export(dts, name.clone(), module)
                    }
                  };
                  for (i, sym) in chain.into_iter().enumerate() {
                    let claim = match bind_rank.get(&sym) {
                      None => true,
                      Some(held) if i == 0 => *held <= BindRank::Default,
                      Some(held) => *held < BindRank::Default,
                    };
                    if claim {
                      sh.rule_symbols.insert(sym, obj);
                      bind_rank.insert(sym, BindRank::Default);
                    }
                  }
                  continue;
                }
                // A public export whose `.d.ts` ignores some arguments (`unknown`/`any`) or bounds
                // its callbacks' parameters (`useSpan`'s `span: Span`) carries them (design
                // section 8).
                let bounds = specs.and_then(|_| {
                let js = self.module_table[module].id().as_str().strip_suffix(".js")?;
                let dts = std::path::PathBuf::from(format!("{js}.d.ts"));
                let callbacks = a.dts.callback_bounds(&dts, name.as_str()).map(|b| {
                  b.into_iter().map(|t| t.map(|t| a.intern_ty(t))).collect::<Vec<_>>()
                });
                let ignored = a.dts.ignored_params(&dts, name.as_str(), 4);
                if callbacks.is_none() && ignored.iter().all(|v| v.iter().all(|x| !x)) {
                  return None;
                }
                if debug {
                  eprintln!(
                    "[effect-service-shaking] default export {} {name}: ignored {ignored:?}, callbacks {}",
                    dts.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                    callbacks.is_some()
                  );
                }
                a.default_bounds.push(heap::DefaultBounds { ignored, callbacks });
                u32::try_from(a.default_bounds.len() - 1).ok()
              });
                (effect::default_object(&mut a, bounds), BindRank::Default)
              }
            }
          }
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
    // The listed exports of otherwise type-modelled modules (`effect::LISTED_MODULES`): their
    // declarations take the rule over the typed object. Should a listed module be analysed under
    // the service-constructor exception, the generator skips them, so the rule still holds.
    for (module, specs) in &listed_modules {
      for (name, spec) in *specs {
        let Some(export) = self.metas[*module].resolved_exports.get(*name) else { continue };
        let obj = effect::spec_object(&mut a, *spec);
        for sym in self.alias_chain(&decls, export.symbol_ref) {
          if sym.owner == *module {
            sh.rule_symbols.insert(sym, obj);
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

    rounds.phases.lap(1);
    phase!("model bound, {} rule symbols", sh.rule_symbols.len());
    if debug {
      for (i, m) in self.module_table.modules.iter().enumerate() {
        eprintln!("[effect-service-shaking] module {i} = {}", m.id());
      }
    }
    // == key constructors, for the key-class test (`Gen::key_heritage`): the symbols bound to
    // effect's own `Context.Service`/`Context.Reference` rules, and each module exporting one
    // under that name (a namespace import of it reaches the constructor by member).
    for (symbol, obj) in &sh.rule_symbols {
      let ObjKind::Builtin { rule, .. } = a.objs[*obj].kind else { continue };
      if ["effect:Context.Service", "effect:Context.Reference"]
        .iter()
        .any(|r| a.rule_id(r) == Some(rule))
      {
        sh.key_ctor_symbols.insert(*symbol);
      }
    }
    for module in order.iter().copied() {
      for name in ["Service", "Reference"] {
        let Some(export) = self.metas[module].resolved_exports.get(name) else { continue };
        if sh.key_ctor_symbols.contains(&self.symbols.canonical_ref_for(export.symbol_ref)) {
          sh.key_ctor_members.insert((module, name));
        }
      }
    }

    // == module holes (reachability scoping, Tim's 10 Nov decision): a module whose static
    // imports never reach `effect` is not analysed. Its exports are its opaque value, what it
    // references from analysed modules and the globals crosses into it, as into a class hole.
    let holes = self.module_holes(&order, &is_effect_module, analysed_holes);
    let mut hole_modules: FxHashMap<heap::ObjId, ModuleIdx> = FxHashMap::default();
    for module in order.iter().copied().filter(|m| holes.contains(m)) {
      let (class, value) = self.module_hole(&mut a, &mut sh, module, &holes, &mut lazy_roots);
      hole_modules.insert(class, module);
      sh.hole_values.insert(module, value);
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
      if holes.contains(&normal.idx) {
        continue;
      }
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
            // A CommonJS module's exports are not linked to its importers' bindings: when it is a
            // hole, they read its value, so what they hand it still crosses into it.
            Module::Normal(_) => match sh.hole_values.get(&target) {
              Some(value) if sh.cjs_modules.contains(&target) => {
                let name = match &import.imported {
                  Specifier::Star => None,
                  Specifier::Literal(l) => Some(ArcStr::from(l.as_str())),
                };
                external_imports.insert(*sym, (*value, name));
              }
              _ => {
                if sh.cjs_modules.contains(&target) {
                  cjs_imports.insert(*sym);
                }
              }
            },
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
        let value = sh.hole_values.get(&normal.idx).copied().unwrap_or(a.top_ext);
        a.seed(elem, value);
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

    // == type bounds (design section 8): the exported functions of the modules analysed under the
    // service-constructor exception, bounded by the `.d.ts` next to each and resolved when first
    // called. They are the only `effect` functions whose bodies are analysed; a call into any
    // other `effect` module goes through the type model, which reads the same declarations.
    for module in order.iter().copied().filter(|m| analysed.contains(m) && !modelled.contains(m)) {
      let dts_path = declarations(module).expect("checked with the declarations");
      let Some(ast) = self.ast_table[module].as_ref() else { continue };
      for (name, f, returned) in exported_functions(ast.program()) {
        a.lazy_bounds.insert((module, f.0), (dts_path.clone(), name.clone(), f.1, false));
        if let Some(r) = returned {
          a.lazy_bounds.entry((module, r.0)).or_insert((dts_path.clone(), name, r.1, true));
        }
      }
    }

    // == solve
    for f in module_fns {
      a.activate(f);
    }
    rounds.phases.lap(2);
    phase!("solving");
    let solve_start = std::time::Instant::now();
    a.solve();
    rounds.phases.lap(3);
    rounds.count += 1;
    rounds.entries += a.total_pts + a.total_copies;
    rounds.steps += a.steps;
    rounds.per_solve.push((a.total_pts + a.total_copies, a.steps));
    rounds.solve += solve_start.elapsed();
    // Refinement: a module hole that a service, a key or a provider reached is analysed in the
    // next solve, so that it keeps no more methods than its code reads.
    let reached: Vec<ModuleIdx> = {
      let mut v: Vec<ModuleIdx> =
        a.bailing_holes().iter().filter_map(|c| hole_modules.get(c).copied()).collect();
      v.sort_unstable();
      v.dedup();
      v
    };
    // Running out of budget is terminal: every method is kept, with no further solve.
    if !reached.is_empty() && !a.bail_all {
      if report_enabled || debug {
        let names: Vec<&str> =
          reached.iter().map(|m| self.module_table[*m].id().as_str()).collect();
        eprintln!(
          "[effect-service-shaking] {} module holes reached by a service, analysed in the next solve: {names:?}",
          reached.len()
        );
      }
      analysed_holes.extend(reached);
      rounds.dts = Some(std::mem::take(&mut a.dts));
      rounds.phases.lap(4);
      drop(a);
      drop(decls);
      rounds.phases.lap(7);
      return true;
    }
    if debug {
      eprintln!(
        "[effect-service-shaking] type bounds: {} exported functions resolved, {} not called, {} default exports bounded; {} .d.ts files, {} ms",
        a.bounded_functions(),
        a.lazy_bounds.len(),
        a.default_bounds.len(),
        a.dts.files_parsed.get(),
        a.dts.resolve_ns.get() / 1_000_000
      );
    }

    let solved = start.elapsed();
    phase!("solved: {}", a.histogram());
    if report_enabled || debug {
      eprintln!(
        "[effect-service-shaking] widened {} variables past {} values",
        a.widened_vars, a.widen_limit
      );
    }
    // Tim's class rule: a service that reaches class code keeps every method.
    if !a.bail_all {
      let bailed = a.class_bail();
      if report_enabled || debug {
        for (key, why) in &bailed {
          eprintln!("[effect-service-shaking] class bail: key {key} kept whole, a service {why}");
        }
      }
    }
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

    // A key declared in `effect` whose services a constructor builds prunes (option (b)) only
    // while no type-modelled code the program calls may read its service: that code reads the
    // services it reaches through the Context invisibly, so the key keeps every method if any
    // called type-modelled module imports, directly or through other `effect` modules, the
    // module declaring the key.
    let mut forced_keys: Vec<String> = vec![];
    if !constructor_keys.is_empty() {
      let mut reach: FxHashSet<ModuleIdx> = FxHashSet::default();
      let mut stack: Vec<ModuleIdx> = a.typed_called_modules.iter().copied().collect();
      while let Some(m) = stack.pop() {
        if !reach.insert(m) {
          continue;
        }
        if let Module::Normal(n) = &self.module_table.modules[m] {
          stack.extend(
            n.ecma_view
              .import_records
              .iter()
              .filter_map(|r| r.resolved_module)
              .filter(|t| is_effect_module(*t)),
          );
        }
      }
      for (key, owner) in &constructor_keys {
        let consumers: Vec<ModuleIdx> = a
          .typed_called_modules
          .iter()
          .copied()
          .filter(|m| {
            let mut seen: FxHashSet<ModuleIdx> = FxHashSet::default();
            let mut st = vec![*m];
            while let Some(x) = st.pop() {
              if x == *owner {
                return true;
              }
              if !seen.insert(x) {
                continue;
              }
              if let Module::Normal(n) = &self.module_table.modules[x] {
                st.extend(
                  n.ecma_view
                    .import_records
                    .iter()
                    .filter_map(|r| r.resolved_module)
                    .filter(|t| is_effect_module(*t)),
                );
              }
            }
            false
          })
          .collect();
        if reach.contains(owner) {
          let mut names: Vec<String> = consumers
            .iter()
            .map(|m| {
              let id = self.module_table[*m].id().as_str();
              id.rsplit_once("/dist/").map_or(id, |(_, f)| f).to_string()
            })
            .collect();
          names.sort();
          forced_keys.push(format!("{key} (read by type-modelled {})", names.join(", ")));
          a.never_prune.insert(key.clone());
        }
      }
    }
    if report_enabled || debug {
      let (admitted, held, tainted) = a.admission_stats;
      eprintln!(
        "[effect-service-shaking] type model: {} type-modelled modules, {} called; admission: {admitted} admitted, {held} held back, {tainted} stand-ins tainted",
        typed_modules.len(),
        a.typed_called_modules.len()
      );
      if debug {
        let names = |m: ModuleIdx| {
          let id = self.module_table[m].id().as_str();
          id.rsplit_once("/dist/").map_or(id, |(_, f)| f).to_string()
        };
        eprintln!("[effect-service-shaking] type census: {}", a.typed_census(&names));
        eprintln!("[effect-service-shaking] origin census: {}", a.origin_census());
      }
      for (file, name, key) in &constructors {
        eprintln!("[effect-service-shaking] service constructor {file}#{name} -> {key}");
      }
      if debug {
        let mut called: Vec<String> = a
          .typed_called_modules
          .iter()
          .map(|m| {
            let id = self.module_table[*m].id().as_str();
            id.rsplit_once("/dist/").map_or(id, |(_, f)| f).to_string()
          })
          .collect();
        called.sort();
        eprintln!("[effect-service-shaking] type-modelled modules called: {called:?}");
      }
      for k in &forced_keys {
        eprintln!("[effect-service-shaking] constructor key kept whole: {k}");
      }
    }

    // == prune
    rounds.phases.lap(4);
    let names: Vec<String> = self.module_table.modules.iter().map(|m| m.id().to_string()).collect();
    let (edits, mut report) =
      prune::plan(&mut a, &sh, &|idx: ModuleIdx| names[idx.index()].clone());
    report.bail_reasons.clone_from(&a.bail_reasons);
    rounds.phases.lap(5);
    phase!("planned {} removals", report.removed.len());
    drop(sh);
    for (module, properties) in &edits.properties {
      let scoping = self.symbols[*module].as_ref().expect("module symbols").scoping();
      if let Some(ast) = self.ast_table[*module].as_mut() {
        prune::apply(ast, scoping, &mut self.stmt_infos[*module], *module, properties);
      }
    }
    rounds.phases.lap(6);

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
        "[effect-service-shaking] {} vars, {} objects, {} entries, {} steps, solve {} ms, total {} ms",
        a.vars.len(),
        a.objs.len(),
        a.total_pts + a.total_copies,
        a.steps,
        solved.as_millis(),
        start.elapsed().as_millis()
      );
      eprintln!(
        "[effect-service-shaking] solves: {} (the line above is the last; vars, objects, entries and steps are its own, solve and total ms run from the first); all solves: {} entries, {} steps, {} ms solving",
        rounds.count,
        rounds.entries,
        rounds.steps,
        rounds.solve.as_millis()
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
    // The report's own printing is not a phase.
    rounds.phases.start();
    drop(a);
    drop(decls);
    drop(edits);
    rounds.phases.lap(7);
    if report_enabled {
      let per_solve: Vec<String> =
        rounds.per_solve.iter().map(|(e, s)| format!("{e} entries / {s} steps")).collect();
      eprintln!(
        "[effect-service-shaking] each solve: [{}]; phases over all solves: {}",
        per_solve.join(", "),
        rounds.phases.report()
      );
    }
    false
  }

  /// The modules left as holes: those whose static imports, followed transitively, never reach a
  /// module of `effect`. Entries and Rolldown's runtime are always analysed.
  fn module_holes(
    &self,
    order: &[ModuleIdx],
    is_effect_module: &dyn Fn(ModuleIdx) -> bool,
    analysed_holes: &FxHashSet<ModuleIdx>,
  ) -> FxHashSet<ModuleIdx> {
    let mut importers: FxHashMap<ModuleIdx, Vec<ModuleIdx>> = FxHashMap::default();
    for m in order.iter().copied() {
      if let Module::Normal(n) = &self.module_table.modules[m] {
        for t in n.ecma_view.import_records.iter().filter_map(|r| r.resolved_module) {
          importers.entry(t).or_default().push(m);
        }
      }
    }
    let mut reaches: FxHashSet<ModuleIdx> =
      order.iter().copied().filter(|m| is_effect_module(*m)).collect();
    let mut stack: Vec<ModuleIdx> = order.iter().copied().filter(|m| reaches.contains(m)).collect();
    while let Some(t) = stack.pop() {
      for i in importers.get(&t).into_iter().flatten() {
        if reaches.insert(*i) {
          stack.push(*i);
        }
      }
    }
    if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_NO_HOLES").is_some() {
      return FxHashSet::default();
    }
    order
      .iter()
      .copied()
      .filter(|m| {
        matches!(self.module_table.modules[*m], Module::Normal(_))
          && !reaches.contains(m)
          && !analysed_holes.contains(m)
          && *m != self.runtime.id()
          && !self.user_defined_entry_modules.contains(m)
          && self.ast_table[*m].is_some()
      })
      .collect()
  }

  /// A module left as a hole (`module_holes`): its body is not generated. It is a class hole
  /// without heritage (`Analysis::hole`): each export it declares is the hole's opaque value,
  /// and the bindings it imports from analysed modules and the globals it names cross into it.
  fn module_hole(
    &self,
    a: &mut Analysis,
    sh: &mut Shared<'_>,
    module: ModuleIdx,
    holes: &FxHashSet<ModuleIdx>,
    lazy_roots: &mut Vec<SymbolRef>,
  ) -> (heap::ObjId, heap::ObjId) {
    let Module::Normal(normal) = &self.module_table.modules[module] else {
      unreachable!("a module hole is a normal module")
    };
    let site = heap::Site { module, start: 0, sub: 0 };
    let proto = a.alloc(ObjKind::Object, Some(site));
    let prototype = a.var();
    a.seed(prototype, proto);
    let class = a.alloc(ObjKind::Class { ctor: None, heritage: None, prototype }, Some(site));
    a.hole(class, proto);
    let op = a.opaque(class);
    for (_, symbol) in self.sorted_exports(module) {
      if self.symbols.canonical_ref_for(symbol).owner == module {
        let v = sh.symbol_var(a, symbol);
        a.seed(v, op);
      }
    }
    let default = normal.ecma_view.default_export_ref;
    if self.symbols.canonical_ref_for(default).owner == module {
      let v = sh.symbol_var(a, default);
      a.seed(v, op);
    }
    let mut imports: Vec<SymbolRef> = normal
      .ecma_view
      .named_imports
      .iter()
      .filter(|(_, import)| {
        normal.ecma_view.import_records[import.record_idx]
          .resolved_module
          .is_some_and(|t| !holes.contains(&t))
      })
      .map(|(sym, _)| *sym)
      .collect();
    imports.sort_unstable_by_key(|s| s.symbol);
    for sym in imports {
      // The module's own top-level code runs when it loads and may call what it imports, so
      // the statements declaring those bindings run too (`lazy_statements`).
      lazy_roots.push(sym);
      let v = sh.symbol_var(a, sym);
      a.constrain(v, Constraint::ClassRef(class, vec![]));
    }
    let scoping = self.symbols[module].as_ref().expect("module symbols").scoping();
    let mut globals: Vec<&str> =
      scoping.root_unresolved_references().keys().map(oxc_str::Ident::as_str).collect();
    globals.sort_unstable();
    for name in globals {
      let v = (sh.resolve_global)(a, &mut sh.globals, name);
      a.constrain(v, Constraint::ClassRef(class, vec![]));
    }
    (class, op)
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
  /// binds the rule to `internal.map`'s declaration too, so a call that reaches the declaration
  /// another way (from a module analysed under the service-constructor exception) hits it.
  fn alias_chain(&self, decls: &Decls, symbol: SymbolRef) -> Vec<SymbolRef> {
    let mut chain = vec![];
    let mut current = self.symbols.canonical_ref_for(symbol);
    for _ in 0..8 {
      if chain.contains(&current) {
        break;
      }
      chain.push(current);
      let Some(next) = self.alias_target(decls, current) else { break };
      current = self.symbols.canonical_ref_for(next);
    }
    chain
  }

  /// Whether the symbol's top-level declaration creates a service key.
  fn key_declaration_of(&self, decls: &Decls, symbol: SymbolRef) -> Option<(Option<String>, bool)> {
    let module = symbol.owner;
    let ast = self.ast_table[module].as_ref()?;
    let program = ast.program();
    if let Some(init) = decls.init(program, module, symbol.symbol) {
      return effect::key_declaration(init);
    }
    // `class K extends Context.Service<..>()("key") {}`
    let class = decls.class(program, module, symbol.symbol)?;
    let heritage = class.heritage.as_ref()?;
    let _: &[ClassElement<'_>] = &class.body.body;
    effect::key_declaration(&heritage.expression)
  }

  /// The key an export holds through its chain of value aliases (`alias_chain`, which already
  /// resolves imports and re-exports to the declarations they name). Every hop must be a
  /// top-level `const`, so no write can change what the export holds: a `let` or `var` on
  /// the way (`export let key = initial; key = 'other'`) leaves it a summary, whatever the
  /// initialiser it started from. The first hop holding a literal key names it.
  fn constant_key_through(&self, chain: &[SymbolRef]) -> Option<String> {
    for symbol in chain {
      let ast = self.ast_table[symbol.owner].as_ref()?;
      find_top_level_const_init(ast.program(), symbol.symbol)?;
      if let Some(key) = self.constant_key_of(*symbol) {
        return Some(key);
      }
    }
    None
  }

  /// The property key a top-level `const` holds when its initialiser is a string literal or
  /// `Symbol.for` of one, named as the model names it (`"@@for:..."` for a registered symbol,
  /// as the `Symbol.for` rule does).
  fn constant_key_of(&self, symbol: SymbolRef) -> Option<String> {
    let ast = self.ast_table[symbol.owner].as_ref()?;
    let scoping = self.symbols[symbol.owner].as_ref()?.scoping();
    let init = find_top_level_const_init(ast.program(), symbol.symbol)?;
    match init.without_parentheses() {
      Expression::StringLiteral(s) => Some(s.value.as_str()?.to_string()),
      Expression::CallExpression(call) => {
        let Expression::StaticMemberExpression(callee) = &call.callee else { return None };
        let Expression::Identifier(object) = &callee.object else { return None };
        // The global `Symbol`, not a binding of that name.
        let global =
          object.reference_id.get().is_some_and(|r| scoping.get_reference(r).symbol_id().is_none());
        if !global || object.name != "Symbol" || callee.property.name != "for" {
          return None;
        }
        match call.arguments.as_slice() {
          [oxc::ast::ast::Argument::StringLiteral(s)] => {
            Some(format!("@@for:{}", s.value.as_str()?))
          }
          _ => None,
        }
      }
      _ => None,
    }
  }

  /// The symbol a top-level `const x = y` or `const x = ns.y` declaration aliases.
  fn alias_target(&self, decls: &Decls, symbol: SymbolRef) -> Option<SymbolRef> {
    let module = symbol.owner;
    let Module::Normal(normal) = &self.module_table.modules[module] else { return None };
    let ast = self.ast_table[module].as_ref()?;
    let scoping = self.symbols[module].as_ref()?.scoping();
    let init = decls.init(ast.program(), module, symbol.symbol)?;
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
        let ns_module = *decls
          .namespaces
          .get_or_init(|| {
            let mut by_symbol = FxHashMap::default();
            for m in &self.module_table.modules {
              if let Module::Normal(n) = m {
                let ns = self.symbols.canonical_ref_for(n.ecma_view.namespace_object_ref);
                by_symbol.entry(ns).or_insert(n.idx);
              }
            }
            by_symbol
          })
          .get(&ns_symbol)?;
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

/// The initialiser of a top-level `const` declaring `symbol`.
fn find_top_level_const_init<'a>(
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
    if decl.kind != oxc::ast::ast::VariableDeclarationKind::Const {
      continue;
    }
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

/// Top-level declarations by symbol, indexed per module on first use, and modules by their
/// canonical namespace symbol. `alias_chain` and `key_declaration_of` ask about every export of
/// a module, so scanning its body (or every module) for each would be quadratic.
#[derive(Default)]
struct Decls {
  modules: RefCell<FxHashMap<ModuleIdx, ModuleDecls>>,
  namespaces: OnceCell<FxHashMap<SymbolRef, ModuleIdx>>,
}

#[derive(Default)]
struct ModuleDecls {
  /// The first top-level `var`, `let` or `const` declarator binding the symbol, as (statement,
  /// declarator).
  vars: FxHashMap<oxc::semantic::SymbolId, (usize, usize)>,
  /// The first top-level class declaration naming the symbol, by statement.
  classes: FxHashMap<oxc::semantic::SymbolId, usize>,
}

impl Decls {
  fn of<R>(
    &self,
    program: &oxc::ast::ast::Program<'_>,
    module: ModuleIdx,
    f: impl FnOnce(&ModuleDecls) -> R,
  ) -> R {
    let mut modules = self.modules.borrow_mut();
    f(modules.entry(module).or_insert_with(|| {
      let mut decls = ModuleDecls::default();
      for (i, stmt) in program.body.iter().enumerate() {
        if let Some(decl) = top_level_var(stmt) {
          for (j, declarator) in decl.declarations.iter().enumerate() {
            if let oxc::ast::ast::BindingPattern::BindingIdentifier(id) = &declarator.id {
              if let Some(symbol) = id.symbol_id.get() {
                decls.vars.entry(symbol).or_insert((i, j));
              }
            }
          }
        } else if let Some(symbol) =
          top_level_class(stmt).and_then(|c| c.id.as_ref()).and_then(|id| id.symbol_id.get())
        {
          decls.classes.entry(symbol).or_insert(i);
        }
      }
      decls
    }))
  }

  /// The initialiser of the symbol's top-level `var`, `let` or `const` declaration, if any.
  fn init<'a>(
    &self,
    program: &'a oxc::ast::ast::Program<'a>,
    module: ModuleIdx,
    symbol: oxc::semantic::SymbolId,
  ) -> Option<&'a Expression<'a>> {
    let (i, j) = self.of(program, module, |d| d.vars.get(&symbol).copied())?;
    top_level_var(&program.body[i])?.declarations[j].init.as_ref()
  }

  /// The symbol's top-level class declaration, if any.
  fn class<'a>(
    &self,
    program: &'a oxc::ast::ast::Program<'a>,
    module: ModuleIdx,
    symbol: oxc::semantic::SymbolId,
  ) -> Option<&'a oxc::ast::ast::Class<'a>> {
    let i = self.of(program, module, |d| d.classes.get(&symbol).copied())?;
    top_level_class(&program.body[i])
  }
}

fn top_level_var<'a>(
  stmt: &'a Statement<'a>,
) -> Option<&'a oxc::ast::ast::VariableDeclaration<'a>> {
  match stmt {
    Statement::VariableDeclaration(d) => Some(d),
    Statement::ExportDeclaration(e) => match &e.declaration {
      oxc::ast::ast::Declaration::VariableDeclaration(d) => Some(d),
      _ => None,
    },
    _ => None,
  }
}

fn top_level_class<'a>(stmt: &'a Statement<'a>) -> Option<&'a oxc::ast::ast::Class<'a>> {
  match stmt {
    Statement::ClassDeclaration(c) => Some(c),
    Statement::ExportDeclaration(e) => match &e.declaration {
      oxc::ast::ast::Declaration::ClassDeclaration(c) => Some(c),
      _ => None,
    },
    Statement::ExportDefaultDeclaration(d) => match &d.declaration {
      oxc::ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(c) => Some(c),
      _ => None,
    },
    _ => None,
  }
}

/// A function: its span start and parameter count.
type FnAt = (u32, usize);

/// The exported functions of a module: export name, the function that runs (for
/// `dual(arity, body)`, the body), and the function literal it returns directly, if any
/// (`return (input) => ...`), whose parameters the declared return type bounds.
fn exported_functions(program: &oxc::ast::ast::Program<'_>) -> Vec<(ArcStr, FnAt, Option<FnAt>)> {
  fn literal(e: &Expression<'_>) -> Option<FnAt> {
    match e.without_parentheses() {
      Expression::ArrowFunctionExpression(f) => Some((f.span.start, f.params.items.len())),
      Expression::FunctionExpression(f) => Some((f.span.start, f.params.items.len())),
      _ => None,
    }
  }
  /// The function literal a body returns, when its only top-level `return` returns one.
  fn returned(body: &oxc::ast::ast::FunctionBody<'_>) -> Option<FnAt> {
    let mut found = None;
    for stmt in &body.statements {
      if let Statement::ReturnStatement(r) = stmt {
        if found.is_some() {
          return None;
        }
        found = Some(literal(r.argument.as_ref()?)?);
      }
    }
    found
  }
  fn with_returned(e: &Expression<'_>) -> Option<(FnAt, Option<FnAt>)> {
    match e.without_parentheses() {
      Expression::ArrowFunctionExpression(f) => {
        let ret = match &f.body {
          oxc::ast::ast::ArrowFunctionBody::FunctionBody(b) => returned(b),
          other => other.as_expression().and_then(literal),
        };
        Some(((f.span.start, f.params.items.len()), ret))
      }
      Expression::FunctionExpression(f) => {
        Some(((f.span.start, f.params.items.len()), f.body.as_ref().and_then(|b| returned(b))))
      }
      _ => None,
    }
  }
  fn function_of(e: &Expression<'_>) -> Option<(FnAt, Option<FnAt>)> {
    match e.without_parentheses() {
      Expression::CallExpression(c) => {
        let is_dual = match &c.callee {
          Expression::Identifier(i) => i.name.as_str() == "dual",
          Expression::StaticMemberExpression(m) => m.property.name.as_str() == "dual",
          _ => false,
        };
        if !is_dual {
          return None;
        }
        with_returned(c.arguments.get(1)?.as_expression()?)
      }
      e => with_returned(e),
    }
  }
  let mut locals: FxHashMap<&str, (FnAt, Option<FnAt>)> = FxHashMap::default();
  let mut exported: Vec<(ArcStr, &str)> = vec![];
  for stmt in &program.body {
    let (decl, is_export) = match stmt {
      Statement::ExportDeclaration(e) => (&e.declaration, true),
      Statement::ExportNamedDeclaration(e) => {
        for sp in &e.specifiers {
          if let oxc::ast::ast::ModuleExportName::IdentifierReference(local) = &sp.local {
            exported.push((ArcStr::from(sp.exported.name().as_str()), local.name.as_str()));
          }
        }
        continue;
      }
      _ => match stmt.as_declaration() {
        Some(d) => (d, false),
        None => continue,
      },
    };
    match decl {
      oxc::ast::ast::Declaration::VariableDeclaration(v) => {
        for d in &v.declarations {
          if let (oxc::ast::ast::BindingPattern::BindingIdentifier(id), Some(init)) =
            (&d.id, &d.init)
          {
            if let Some(f) = function_of(init) {
              locals.insert(id.name.as_str(), f);
              if is_export {
                exported.push((ArcStr::from(id.name.as_str()), id.name.as_str()));
              }
            }
          }
        }
      }
      oxc::ast::ast::Declaration::FunctionDeclaration(f) => {
        if let Some(id) = &f.id {
          let ret = f.body.as_ref().and_then(|b| returned(b));
          locals.insert(id.name.as_str(), ((f.span.start, f.params.items.len()), ret));
          if is_export {
            exported.push((ArcStr::from(id.name.as_str()), id.name.as_str()));
          }
        }
      }
      _ => {}
    }
  }
  exported
    .into_iter()
    .filter_map(|(name, local)| locals.get(local).map(|&(f, ret)| (name, f, ret)))
    .collect()
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
  use std::sync::atomic::Ordering;
  use std::sync::{Arc, Mutex, mpsc};

  use super::{EffectDeclarations, Loading, Preload, dts};

  /// A pass that ends at a contract check (`effect` external, an unsupported version, missing
  /// exports) drops the declarations it never adopted. The load is held unfinished across that
  /// drop: the drop must ask it to stop and return only after it has ended.
  #[test]
  fn abandoned_preload_ends_before_the_pass_returns() {
    let log: Arc<Mutex<Vec<&str>>> = Arc::default();
    let (release, gate) = mpsc::channel::<()>();
    let saw_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let preload = {
      let (log, saw_cancel) = (Arc::clone(&log), Arc::clone(&saw_cancel));
      Preload::start(vec![], move |paths, cancel| {
        gate.recv().expect("released");
        saw_cancel.store(cancel.load(Ordering::Relaxed), Ordering::Relaxed);
        log.lock().unwrap().push("load ended");
        dts::load(paths, cancel)
      })
    };
    let cancel = Arc::clone(&preload.cancel);
    let declarations = EffectDeclarations { modules: vec![], loading: Loading::Started(preload) };
    // The early return, on its own thread so that this one can hold the load.
    let pass = {
      let log = Arc::clone(&log);
      std::thread::spawn(move || {
        drop(declarations);
        log.lock().unwrap().push("pass returned");
      })
    };
    let begun = std::time::Instant::now();
    while !cancel.load(Ordering::Relaxed) && !log.lock().unwrap().contains(&"pass returned") {
      assert!(begun.elapsed().as_secs() < 30, "the drop neither cancelled nor returned");
      std::thread::sleep(std::time::Duration::from_millis(1));
    }
    release.send(()).expect("the load waits");
    pass.join().expect("the pass thread");
    assert_eq!(*log.lock().unwrap(), ["load ended", "pass returned"]);
    assert!(saw_cancel.load(Ordering::Relaxed), "the load was asked to stop");
  }
}
