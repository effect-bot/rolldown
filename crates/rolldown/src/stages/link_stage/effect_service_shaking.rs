//! Experimental removal of Effect service methods that no consumer reads.
//!
//! Enabled with `experimental.effectServiceShaking`. The pass runs in the link
//! stage, after symbols are bound and before statements are included, and
//! works on the module ASTs with oxc's scoping and Rolldown's symbol database.
//!
//! Base case only:
//! - Service keys are module-level `const K = Context.Service(...)` declarations
//!   with a literal key string.
//! - Consumers are `const s = yield* K` (or `const { a } = yield* K`) inside an
//!   `Effect.gen` / `Effect.fn` generator, where `s` is only read with static
//!   member access.
//! - Providers are `Layer.succeed(K, {...})` and `Layer.effect(K, e)` where `e` is
//!   built from `Effect.gen`, `Effect.sync`, `Effect.succeed` and
//!   `Effect.acquireRelease`, and returns `K.of({...})` or `{...}`.
//!
//! Demand is attached to the key string. Reads inside a removed method, or
//! inside a constructor-local function only that method used, do not count:
//! demand is a least fixpoint over code that can still run. Code that runs
//! while a provider is constructed always counts, and the pass never removes
//! statements other than unused local `const` functions, so acquisition,
//! release and construction-time calls keep running.
//!
//! Anything outside the base case keeps every method of the key involved, and
//! anything that could reach a Context or Fiber from outside the analysis
//! (entry exports, external modules, reflection, unaudited Effect APIs)
//! disables the pass for the whole build.

use oxc::{
  allocator::Vec as ArenaVec,
  ast::{
    AstKind,
    ast::{
      Argument, BindingPattern, CallExpression, Expression, Function, FunctionType,
      IdentifierReference, ImportExpression, ObjectExpression, ObjectPropertyKind, PropertyKind,
      Statement, StaticMemberExpression, VariableDeclarationKind, VariableDeclarator,
      YieldExpression,
    },
  },
  ast_visit::{VisitJs, VisitJsMut, walk_js, walk_js_mut},
  semantic::{ReferenceId, Scoping, SymbolId},
  span::{GetSpan, Span},
};
use rolldown_common::{
  GetLocalDb, ModuleIdx, StmtInfoIdx, SymbolOrMemberExprRef, SymbolRef, SymbolRefDb,
};
use rustc_hash::{FxHashMap, FxHashSet};

use super::LinkStage;

// Trusted APIs that code outside the trusted packages may reference. Any other
// export of a trusted package may hand a Fiber or Context to user code, through
// which every service can be retrieved by its key string, so it disables the
// pass. Mirrors the audited list of the `@effect/bundle` prototype.
const ALLOWED_APIS: &[&str] = &[
  "Effect.gen",
  "Effect.fn",
  "Effect.fnUntraced",
  "Effect.succeed",
  "Effect.sync",
  "Effect.fail",
  "Effect.die",
  "Effect.promise",
  "Effect.tryPromise",
  "Effect.suspend",
  "Effect.map",
  "Effect.flatMap",
  "Effect.andThen",
  "Effect.tap",
  "Effect.as",
  "Effect.asVoid",
  "Effect.all",
  "Effect.forEach",
  "Effect.catchTag",
  "Effect.catchTags",
  "Effect.orDie",
  "Effect.matchEffect",
  "Effect.match",
  "Effect.provide",
  "Effect.provideService",
  "Effect.provideServiceEffect",
  "Effect.updateService",
  "Effect.service",
  "Effect.serviceOption",
  "Effect.scoped",
  "Effect.acquireRelease",
  "Effect.acquireUseRelease",
  "Effect.addFinalizer",
  "Effect.ensuring",
  "Effect.onExit",
  "Effect.log",
  "Effect.logInfo",
  "Effect.logDebug",
  "Effect.logWarning",
  "Effect.logError",
  "Effect.annotateLogs",
  "Effect.withLogSpan",
  "Effect.sleep",
  "Effect.yieldNow",
  "Effect.callback",
  "Effect.exit",
  "Effect.when",
  "Effect.zip",
  "Effect.zipWith",
  "Effect.flatten",
  "Effect.isEffect",
  "Effect.ignore",
  "Effect.option",
  "Effect.result",
  "Effect.timeout",
  "Effect.delay",
  "Layer.succeed",
  "Layer.effect",
  "Layer.sync",
  "Layer.merge",
  "Layer.mergeAll",
  "Layer.provide",
  "Layer.provideMerge",
  "Layer.launch",
  "Layer.empty",
  "Layer.effectDiscard",
  "Context.Service",
  "Context.Reference",
  "Context.get",
  "Context.getUnsafe",
  "Context.getOrElse",
  "Context.getOrUndefined",
  "Context.getOption",
  "Context.isContext",
  "FileSystem.FileSystem",
  "FileSystem.make",
  "FileSystem.makeNoop",
  "FileSystem.layerNoop",
  "@effect/platform-node/NodeFileSystem.layer",
  "@effect/platform-node/NodeServices.layer",
  "@effect/platform-node/NodeRuntime.runMain",
];

// `RunOptions` can install `onFiberStart`, so these are only allowed with the
// effect as the single argument, and `runFork` only when its Fiber is discarded.
const RUN_APIS: &[&str] =
  &["Effect.runPromise", "Effect.runPromiseExit", "Effect.runSync", "Effect.runSyncExit"];

// Data modules whose declarations mention no Fiber, Context or runtime type.
const ALLOWED_MODULES: &[&str] = &[
  "Option",
  "Array",
  "Order",
  "Equivalence",
  "Equal",
  "Hash",
  "Data",
  "Predicate",
  "String",
  "Number",
  "BigInt",
  "Boolean",
  "Struct",
  "Record",
  "Tuple",
  "Duration",
  "Chunk",
  "HashMap",
  "HashSet",
  "Result",
  "Redacted",
  "Brand",
  "Function",
  "Iterable",
  "Match",
];

// Globals that reach the global object, where the running Fiber is published,
// or that run code that can.
const REFLECTIVE_GLOBALS: &[&str] = &[
  "globalThis",
  "global",
  "self",
  "window",
  "frames",
  "parent",
  "top",
  "eval",
  "Function",
  "Reflect",
  "Proxy",
  "require",
];
const ALLOWED_SYMBOL_MEMBERS: &[&str] = &["iterator", "asyncIterator"];
const ALLOWED_OBJECT_MEMBERS: &[&str] =
  &["keys", "values", "entries", "fromEntries", "assign", "freeze", "is"];
// Reading one of these on a service observes the whole object.
const INHERITED_MEMBERS: &[&str] = &[
  "constructor",
  "hasOwnProperty",
  "isPrototypeOf",
  "propertyIsEnumerable",
  "toLocaleString",
  "toString",
  "valueOf",
  "__proto__",
  "__defineGetter__",
  "__defineSetter__",
  "__lookupGetter__",
  "__lookupSetter__",
];
const NODE_BUILTINS: &[&str] = &[
  "assert",
  "assert/strict",
  "async_hooks",
  "buffer",
  "child_process",
  "cluster",
  "console",
  "constants",
  "crypto",
  "dgram",
  "diagnostics_channel",
  "dns",
  "dns/promises",
  "domain",
  "events",
  "fs",
  "fs/promises",
  "http",
  "http2",
  "https",
  "inspector",
  "module",
  "net",
  "os",
  "path",
  "path/posix",
  "path/win32",
  "perf_hooks",
  "process",
  "punycode",
  "querystring",
  "readline",
  "readline/promises",
  "repl",
  "stream",
  "stream/promises",
  "stream/web",
  "string_decoder",
  "timers",
  "timers/promises",
  "tls",
  "trace_events",
  "tty",
  "url",
  "util",
  "util/types",
  "v8",
  "vm",
  "wasi",
  "worker_threads",
  "zlib",
];

/// How a trusted module's exports are named in `ALLOWED_APIS`: `Effect.map` for
/// the effect package, `@effect/platform-node/NodeRuntime.runMain` otherwise.
fn trusted_prefix(id: &str) -> Option<String> {
  let id = id.replace('\\', "/");
  let markers: &[(&str, &str)] = &[
    ("/node_modules/effect/", "effect"),
    ("/packages/effect/", "effect"),
    ("/node_modules/@effect/platform-node/", "@effect/platform-node"),
    ("/packages/platform/node/", "@effect/platform-node"),
    ("/node_modules/@effect/platform-node-shared/", "@effect/platform-node-shared"),
    ("/packages/platform/node-shared/", "@effect/platform-node-shared"),
  ];
  for (marker, package) in markers {
    let Some(index) = id.rfind(marker) else { continue };
    let mut rest = &id[index + marker.len()..];
    for prefix in ["src/", "dist/esm/", "dist/"] {
      if let Some(stripped) = rest.strip_prefix(prefix) {
        rest = stripped;
        break;
      }
    }
    let rest = rest.rsplit_once('.').map_or(rest, |(stem, _)| stem);
    return Some(if *package == "effect" { rest.to_string() } else { format!("{package}/{rest}") });
  }
  None
}

fn strip<'b, 'a>(expr: &'b Expression<'a>) -> &'b Expression<'a> {
  let mut expr = expr;
  loop {
    expr = match expr {
      Expression::ParenthesizedExpression(e) => &e.expression,
      Expression::TSAsExpression(e) => &e.expression,
      Expression::TSSatisfiesExpression(e) => &e.expression,
      Expression::TSNonNullExpression(e) => &e.expression,
      Expression::TSTypeAssertion(e) => &e.expression,
      _ => return expr,
    }
  }
}

enum KeyCreation {
  Literal(String),
  Dynamic,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Resolved {
  Symbol(SymbolRef),
  Namespace(ModuleIdx),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Guard {
  /// A property of a provider object literal: (site, property index).
  Property(usize, usize),
  /// A constructor-local `const` function.
  Binding(SymbolRef),
}

/// Where a Layer or Context built from a provider ends up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Container {
  /// Consumed by `Effect.provide` or `Layer.launch`.
  Terminal,
  /// Stored in a `const`, whose uses are checked in turn.
  Binding(SymbolRef),
  Escape,
}

#[derive(Debug)]
struct RefInfo {
  span: Span,
  top_stmt: usize,
  chain: Vec<Guard>,
  /// Static member read on the reference: (name, is a write target).
  member: Option<(String, bool)>,
  container: Container,
  /// The root-scope symbol referenced, for statement bookkeeping.
  root_symbol: Option<SymbolId>,
}

#[derive(Debug)]
struct Prop {
  name: Option<String>,
  span: Span,
  prunable: bool,
}

#[derive(Debug)]
struct Site {
  module: ModuleIdx,
  keys: FxHashSet<String>,
  props: Vec<Prop>,
  /// Keep every property, e.g. a method may read `this`.
  top: bool,
  /// Locals bound to the provider object before it reaches a Context, such as
  /// an `acquireRelease` release callback parameter.
  bindings: Vec<SymbolRef>,
}

/// Reads of a provider object through locals bound before it reaches a
/// Context: (site, (name, guards) list, escaped).
type SiteBindingReads = (usize, Vec<(String, Vec<Guard>)>, bool);

#[derive(Debug)]
enum ConsumerKind {
  Binding(SymbolRef),
  Names(Vec<String>),
}

#[derive(Debug)]
struct Consumer {
  key: String,
  kind: ConsumerKind,
  chain: Vec<Guard>,
}

#[derive(Debug)]
struct GuardBinding {
  symbol: SymbolRef,
  declarator: Span,
}

#[derive(Default, Debug)]
struct Demand {
  all: bool,
  names: FxHashSet<String>,
}

impl Demand {
  fn add(&mut self, name: &str) -> bool {
    if self.all || self.names.contains(name) {
      return false;
    }
    self.names.insert(name.to_string());
    true
  }
  fn add_all(&mut self, other: &Demand) -> bool {
    if self.all {
      return false;
    }
    if other.all {
      self.all = true;
      return true;
    }
    let mut changed = false;
    for name in &other.names {
      changed |= self.add(name);
    }
    changed
  }
  fn set_all(&mut self) -> bool {
    let changed = !self.all;
    self.all = true;
    changed
  }
  fn has(&self, name: &str) -> bool {
    self.all || self.names.contains(name)
  }
}

/// Outcome of the pass, printed when `ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT` is set.
#[derive(Default, Debug)]
struct Report {
  global_bailouts: Vec<String>,
  key_bailouts: Vec<(String, String)>,
  removed: Vec<String>,
}

/// Facts shared by every module visit.
struct Shared<'l> {
  symbols: &'l SymbolRefDb,
  namespaces: FxHashMap<SymbolRef, ModuleIdx>,
  resolved_exports: Vec<FxHashMap<String, SymbolRef>>,
  prefixes: Vec<Option<String>>,
  /// API names (`Effect.map`) of the exports of trusted modules.
  api_names: FxHashMap<SymbolRef, String>,
  keys: FxHashMap<SymbolRef, String>,
  key_creations: FxHashSet<(ModuleIdx, u32)>,
  this_free: FxHashSet<SymbolRef>,
}

impl Shared<'_> {
  fn scoping(&self, module: ModuleIdx) -> &Scoping {
    self.symbols.local_db(module).scoping()
  }

  fn as_resolved(&self, symbol: SymbolRef) -> Resolved {
    match self.namespaces.get(&symbol) {
      Some(module) => Resolved::Namespace(*module),
      None => Resolved::Symbol(symbol),
    }
  }

  fn resolve_ident(&self, module: ModuleIdx, ident: &IdentifierReference<'_>) -> Option<Resolved> {
    let reference_id = ident.reference_id.get()?;
    let symbol = self.scoping(module).get_reference(reference_id).symbol_id()?;
    Some(self.as_resolved(self.symbols.canonical_ref_for((module, symbol).into())))
  }

  fn resolve_export(&self, module: ModuleIdx, name: &str) -> Option<Resolved> {
    let symbol = self.resolved_exports.get(module.index())?.get(name)?;
    Some(self.as_resolved(self.symbols.canonical_ref_for(*symbol)))
  }

  fn resolve_expr(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<Resolved> {
    match strip(expr) {
      Expression::Identifier(ident) => self.resolve_ident(module, ident),
      Expression::StaticMemberExpression(member) => {
        let Resolved::Namespace(target) = self.resolve_expr(module, &member.object)? else {
          return None;
        };
        self.resolve_export(target, member.property.name.as_str())
      }
      _ => None,
    }
  }

  fn api_name(&self, resolved: Option<Resolved>) -> Option<&str> {
    let Resolved::Symbol(symbol) = resolved? else { return None };
    self.api_names.get(&symbol).map(String::as_str)
  }

  fn api_of(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<&str> {
    self.api_name(self.resolve_expr(module, expr))
  }

  fn key_of(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<&String> {
    match self.resolve_expr(module, expr)? {
      Resolved::Symbol(symbol) => self.keys.get(&symbol),
      Resolved::Namespace(_) => None,
    }
  }

  /// A key creation `Context.Service("key")` or `Context.Service<..>()("key")`:
  /// `None` when the expression does not create a key.
  fn key_creation(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<KeyCreation> {
    let Expression::CallExpression(call) = strip(expr) else { return None };
    let callee = strip(&call.callee);
    let api = self.api_of(module, callee);
    let direct = matches!(api, Some("Context.Service" | "Context.Reference"));
    let curried = matches!(callee, Expression::CallExpression(inner)
      if inner.arguments.is_empty()
        && self.api_of(module, &inner.callee) == Some("Context.Service"));
    if !direct && !curried {
      return None;
    }
    Some(match call.arguments.first() {
      Some(Argument::StringLiteral(literal)) => literal
        .value
        .as_str()
        .map_or(KeyCreation::Dynamic, |key| KeyCreation::Literal(key.to_string())),
      _ => KeyCreation::Dynamic,
    })
  }
}

fn contains_this(function_body: &oxc::ast::ast::FunctionBody<'_>) -> bool {
  struct Finder(bool);
  impl<'a> VisitJs<'a> for Finder {
    fn visit_this_expression(&mut self, _it: &oxc::ast::ast::ThisExpression) {
      self.0 = true;
    }
    fn visit_function(&mut self, _it: &Function<'a>, _flags: oxc::semantic::ScopeFlags) {}
  }
  let mut finder = Finder(false);
  finder.visit_function_body(function_body);
  finder.0
}

/// Whether a function body reads its own `arguments`, which aliases every
/// parameter without going through them.
fn reads_arguments(function_body: &oxc::ast::ast::FunctionBody<'_>) -> bool {
  struct Finder(bool);
  impl<'a> VisitJs<'a> for Finder {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
      self.0 |= it.name == "arguments";
    }
    fn visit_function(&mut self, _it: &Function<'a>, _flags: oxc::semantic::ScopeFlags) {}
  }
  let mut finder = Finder(false);
  finder.visit_function_body(function_body);
  finder.0
}

/// Return values of a function body, and whether every return was followed.
struct Returns<'b, 'a>(Vec<&'b Expression<'a>>, bool);
impl<'b, 'a> Returns<'b, 'a> {
  fn statements(&mut self, statements: &'b [Statement<'a>]) {
    for statement in statements {
      self.statement(statement);
    }
  }
  fn statement(&mut self, statement: &'b Statement<'a>) {
    match statement {
      Statement::ReturnStatement(ret) => match &ret.argument {
        Some(argument) => self.0.push(argument),
        None => self.1 = false,
      },
      Statement::BlockStatement(block) => self.statements(&block.body),
      Statement::IfStatement(stmt) => {
        self.statement(&stmt.consequent);
        if let Some(alternate) = &stmt.alternate {
          self.statement(alternate);
        }
      }
      Statement::TryStatement(stmt) => {
        self.statements(&stmt.block.body);
        if let Some(handler) = &stmt.handler {
          self.statements(&handler.body.body);
        }
        if let Some(finalizer) = &stmt.finalizer {
          self.statements(&finalizer.body);
        }
      }
      // Returns nested elsewhere (loops, switch, labels) are not followed.
      Statement::ForStatement(_)
      | Statement::ForInStatement(_)
      | Statement::ForOfStatement(_)
      | Statement::WhileStatement(_)
      | Statement::DoWhileStatement(_)
      | Statement::SwitchStatement(_)
      | Statement::LabeledStatement(_)
      | Statement::WithStatement(_) => self.1 = false,
      _ => {}
    }
  }
}

/// Pass A: service keys and functions that never read `this`.
struct KeyCollector<'s, 'l> {
  shared: &'s Shared<'l>,
  module: ModuleIdx,
  keys: Vec<(SymbolRef, KeyCreation, u32)>,
  this_free: Vec<SymbolRef>,
  depth: usize,
  kind: VariableDeclarationKind,
}

impl<'a> VisitJs<'a> for KeyCollector<'_, '_> {
  fn visit_function(&mut self, it: &Function<'a>, flags: oxc::semantic::ScopeFlags) {
    if let (Some(id), Some(body)) = (&it.id, &it.body) {
      if matches!(it.r#type, FunctionType::FunctionDeclaration) && !contains_this(body) {
        if let Some(symbol) = id.symbol_id.get() {
          self.this_free.push((self.module, symbol).into());
        }
      }
    }
    self.depth += 1;
    walk_js::walk_function(self, it, flags);
    self.depth -= 1;
  }

  fn visit_arrow_function_expression(&mut self, it: &oxc::ast::ast::ArrowFunctionExpression<'a>) {
    self.depth += 1;
    walk_js::walk_arrow_function_expression(self, it);
    self.depth -= 1;
  }

  fn visit_variable_declaration(&mut self, it: &oxc::ast::ast::VariableDeclaration<'a>) {
    let previous = self.kind;
    self.kind = it.kind;
    walk_js::walk_variable_declaration(self, it);
    self.kind = previous;
  }

  fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
    if let (BindingPattern::BindingIdentifier(id), Some(init)) = (&it.id, &it.init) {
      if let Some(symbol) = id.symbol_id.get() {
        let symbol_ref: SymbolRef = (self.module, symbol).into();
        if self.kind == VariableDeclarationKind::Const {
          match strip(init) {
            Expression::ArrowFunctionExpression(_) => self.this_free.push(symbol_ref),
            Expression::FunctionExpression(function)
              if function.body.as_ref().is_some_and(|body| !contains_this(body)) =>
            {
              self.this_free.push(symbol_ref);
            }
            _ => {}
          }
          if self.depth == 0 {
            if let Some(key) = self.shared.key_creation(self.module, init) {
              let Expression::CallExpression(call) = strip(init) else { unreachable!() };
              self.keys.push((symbol_ref, key, call.callee.span().start));
            }
          }
        }
      }
    }
    walk_js::walk_variable_declarator(self, it);
  }
}

/// Pass B: every use of keys, providers, consumers, guards and bailouts.
struct UseCollector<'s, 'l, 'a> {
  shared: &'s Shared<'l>,
  module: ModuleIdx,
  trusted: bool,
  path: Vec<AstKind<'a>>,
  guards: Vec<Guard>,
  top_stmt: usize,
  refs: FxHashMap<ReferenceId, RefInfo>,
  /// Use-expression spans of key references already classified.
  classified: FxHashSet<u32>,
  /// Object literal spans recognised as provider sites -> site index.
  site_spans: FxHashMap<u32, usize>,
  sites: &'s mut Vec<Site>,
  consumers: Vec<Consumer>,
  providers: Vec<(String, Container)>,
  guard_bindings: Vec<GuardBinding>,
  global_bailouts: Vec<String>,
  key_bailouts: Vec<(String, String)>,
}

impl<'a> UseCollector<'_, '_, 'a> {
  fn bail_global(&mut self, span: Span, reason: &str) {
    self.global_bailouts.push(format!("{reason} (module {:?} at {})", self.module, span.start));
  }

  fn bail_key(&mut self, key: &str, span: Span, reason: &str) {
    self
      .key_bailouts
      .push((key.to_string(), format!("{reason} (module {:?} at {})", self.module, span.start)));
  }

  /// Ancestors of the current node, innermost first, skipping wrappers.
  fn parent(&self, depth: usize) -> Option<&AstKind<'a>> {
    self.path.iter().rev().nth(depth)
  }

  /// Where a container value at `span`, whose parent is `self.path[index]`, flows.
  fn container_from(&self, span: Span, mut index: usize) -> Container {
    let mut current = span;
    loop {
      let Some(parent) = self.path.get(index) else { return Container::Escape };
      match parent {
        AstKind::ParenthesizedExpression(_) => {
          current = parent.span();
        }
        AstKind::CallExpression(call) => {
          if call.callee.span() == current {
            return Container::Escape;
          }
          let api = self.shared.api_of(self.module, &call.callee);
          match api {
            Some("Effect.provide" | "Layer.launch") => return Container::Terminal,
            Some("Layer.merge" | "Layer.mergeAll" | "Layer.provide" | "Layer.provideMerge") => {
              if call.arguments.len() == 1 && api != Some("Layer.mergeAll") {
                // A curried combinator must be applied right away.
                let Some(AstKind::CallExpression(outer)) =
                  index.checked_sub(1).and_then(|i| self.path.get(i))
                else {
                  return Container::Escape;
                };
                if outer.callee.span() != call.span {
                  return Container::Escape;
                }
                current = outer.span;
                index -= 1;
              } else {
                current = call.span;
              }
            }
            _ => return Container::Escape,
          }
        }
        AstKind::VariableDeclarator(declarator) => {
          let is_init = declarator.init.as_ref().is_some_and(|init| init.span() == current);
          let kind = match index.checked_sub(1).and_then(|i| self.path.get(i)) {
            Some(AstKind::VariableDeclaration(declaration)) => declaration.kind,
            _ => VariableDeclarationKind::Var,
          };
          return match (&declarator.id, is_init, kind) {
            (BindingPattern::BindingIdentifier(id), true, VariableDeclarationKind::Const) => {
              id.symbol_id.get().map_or(Container::Escape, |symbol| {
                Container::Binding(
                  self.shared.symbols.canonical_ref_for((self.module, symbol).into()),
                )
              })
            }
            _ => Container::Escape,
          };
        }
        _ => return Container::Escape,
      }
      let Some(next) = index.checked_sub(1) else { return Container::Escape };
      index = next;
    }
  }

  /// Provider objects reachable from an Effect expression in the base case.
  /// Returns false when some source is not a recognised object literal.
  fn effect_sites(
    &self,
    expr: &Expression<'a>,
    found: &mut Vec<(u32, Option<SymbolRef>, bool)>,
  ) -> bool {
    let Expression::CallExpression(call) = strip(expr) else { return false };
    let args = &call.arguments;
    match self.shared.api_of(self.module, &call.callee) {
      Some("Effect.succeed") if args.len() == 1 => {
        args[0].as_expression().is_some_and(|value| self.value_sites(value, found))
      }
      Some("Effect.sync") if args.len() == 1 => {
        args[0].as_expression().is_some_and(|f| self.return_sites(f, false, found))
      }
      Some("Effect.gen") if !args.is_empty() => {
        args[args.len() - 1].as_expression().is_some_and(|f| self.return_sites(f, true, found))
      }
      Some("Effect.acquireRelease") if args.len() == 2 => {
        let mut acquired = vec![];
        let complete = args[0].as_expression().is_some_and(|e| self.effect_sites(e, &mut acquired));
        // The release callback receives the acquired object.
        let (binding, top) = match args[1].as_expression().map(strip) {
          Some(Expression::ArrowFunctionExpression(f)) => match f.params.items.first() {
            None => (None, false),
            Some(param) => match &param.pattern {
              BindingPattern::BindingIdentifier(id) => {
                (id.symbol_id.get().map(|s| (self.module, s).into()), false)
              }
              _ => (None, true),
            },
          },
          Some(Expression::FunctionExpression(f)) => {
            let reads_arguments = f.body.as_ref().is_none_or(|body| reads_arguments(body));
            match f.params.items.first() {
              None if !reads_arguments => (None, false),
              Some(param) if !reads_arguments => match &param.pattern {
                BindingPattern::BindingIdentifier(id) => {
                  (id.symbol_id.get().map(|s| (self.module, s).into()), false)
                }
                _ => (None, true),
              },
              _ => (None, true),
            }
          }
          _ => (None, true),
        };
        for (span, _, existing_top) in acquired {
          found.push((span, binding, existing_top || top));
        }
        complete
      }
      _ => false,
    }
  }

  fn return_sites(
    &self,
    function: &Expression<'a>,
    generator: bool,
    found: &mut Vec<(u32, Option<SymbolRef>, bool)>,
  ) -> bool {
    let body = match strip(function) {
      Expression::ArrowFunctionExpression(f) if !generator && !f.r#async => {
        match f.body.as_function_body() {
          Some(body) => body,
          None => return f.get_expression().is_some_and(|value| self.value_sites(value, found)),
        }
      }
      Expression::FunctionExpression(f) if f.generator == generator && !f.r#async => {
        let Some(body) = &f.body else { return false };
        body
      }
      _ => return false,
    };
    let mut returns = Returns(vec![], true);
    returns.statements(&body.statements);
    let mut complete = returns.1 && !returns.0.is_empty();
    for value in returns.0 {
      complete &= self.value_sites(value, found);
    }
    complete
  }

  fn value_sites(
    &self,
    value: &Expression<'a>,
    found: &mut Vec<(u32, Option<SymbolRef>, bool)>,
  ) -> bool {
    match strip(value) {
      Expression::ObjectExpression(object) => {
        found.push((object.span.start, None, false));
        true
      }
      Expression::CallExpression(call) => {
        // `Key.of(value)` is the identity.
        let Expression::StaticMemberExpression(member) = strip(&call.callee) else { return false };
        if member.property.name != "of" || self.shared.key_of(self.module, &member.object).is_none()
        {
          return false;
        }
        match call.arguments.first().and_then(Argument::as_expression) {
          Some(argument) if call.arguments.len() == 1 => self.value_sites(argument, found),
          _ => false,
        }
      }
      _ => false,
    }
  }

  /// Registers the provider objects of `Layer.succeed(K, value)` / `Layer.effect(K, effect)`.
  fn provider(
    &mut self,
    key: &str,
    kind: &str,
    value: &Expression<'a>,
    container: Container,
    span: Span,
  ) {
    let mut found = vec![];
    let _complete = if kind == "Layer.succeed" {
      self.value_sites(value, &mut found)
    } else {
      self.effect_sites(value, &mut found)
    };
    for (start, binding, top) in found {
      let index = *self.site_spans.entry(start).or_insert_with(|| {
        self.sites.push(Site {
          module: self.module,
          keys: FxHashSet::default(),
          props: vec![],
          top: false,
          bindings: vec![],
        });
        self.sites.len() - 1
      });
      let site = &mut self.sites[index];
      site.keys.insert(key.to_string());
      site.top |= top;
      if let Some(binding) = binding {
        site.bindings.push(binding);
      }
    }
    self.providers.push((key.to_string(), container));
    let _ = span;
  }

  fn is_effect_generator(&self, yield_depth: usize) -> bool {
    // The nearest function around the `yield*` must be a generator passed to
    // `Effect.gen`, `Effect.fn` or `Effect.fnUntraced`.
    let mut iter = self.path.iter().rev().skip(yield_depth);
    while let Some(kind) = iter.next() {
      match kind {
        AstKind::Function(function) => {
          if !function.generator {
            return false;
          }
          return match iter.next() {
            Some(AstKind::CallExpression(call)) => {
              matches!(
                self.shared.api_of(self.module, &call.callee),
                Some("Effect.gen" | "Effect.fn" | "Effect.fnUntraced")
              ) && call.arguments.iter().any(|arg| arg.span() == function.span)
            }
            _ => false,
          };
        }
        AstKind::ArrowFunctionExpression(_) => return false,
        _ => {}
      }
    }
    false
  }

  fn record_reference(&mut self, ident: &IdentifierReference<'a>) {
    let Some(reference_id) = ident.reference_id.get() else { return };
    let scoping = self.shared.scoping(self.module);
    let symbol = scoping.get_reference(reference_id).symbol_id();
    let root_symbol =
      symbol.filter(|symbol| scoping.symbol_scope_id(*symbol) == scoping.root_scope_id());
    // Member reads only matter for locals: consumer and release bindings.
    let member = match self.parent(0) {
      Some(AstKind::StaticMemberExpression(member))
        if member.object.span() == ident.span && symbol.is_some() && root_symbol.is_none() =>
      {
        Some((member.property.name.as_str().to_string(), self.is_write_target(1)))
      }
      _ => None,
    };
    let container = self.container_from(ident.span, self.path.len() - 1);
    self.refs.insert(
      reference_id,
      RefInfo {
        span: ident.span,
        top_stmt: self.top_stmt,
        chain: self.guards.clone(),
        member,
        container,
        root_symbol,
      },
    );
  }

  /// Whether the member expression at ancestor `depth - 1` is written.
  fn is_write_target(&self, depth: usize) -> bool {
    match self.parent(depth) {
      Some(
        AstKind::AssignmentExpression(_)
        | AstKind::UpdateExpression(_)
        | AstKind::ArrayAssignmentTarget(_)
        | AstKind::ObjectAssignmentTarget(_)
        | AstKind::AssignmentTargetWithDefault(_)
        | AstKind::AssignmentTargetPropertyProperty(_)
        | AstKind::AssignmentTargetRest(_)
        | AstKind::ForInStatement(_)
        | AstKind::ForOfStatement(_),
      ) => {
        // Reads on the right-hand side of an assignment are still reads.
        match self.parent(depth) {
          Some(AstKind::AssignmentExpression(assign)) => {
            let member = self.parent(depth - 1).map(GetSpan::span);
            member.is_some_and(|span| assign.left.span().contains_inclusive(span))
          }
          Some(AstKind::ForInStatement(stmt)) => {
            self.parent(depth - 1).is_some_and(|m| stmt.left.span().contains_inclusive(m.span()))
          }
          Some(AstKind::ForOfStatement(stmt)) => {
            self.parent(depth - 1).is_some_and(|m| stmt.left.span().contains_inclusive(m.span()))
          }
          _ => true,
        }
      }
      Some(AstKind::UnaryExpression(unary)) => unary.operator.is_delete(),
      _ => false,
    }
  }

  fn check_untrusted_reference(
    &mut self,
    ident: &IdentifierReference<'a>,
    resolved: Option<Resolved>,
  ) {
    let name = ident.name.as_str();
    let member = match self.parent(0) {
      Some(AstKind::StaticMemberExpression(member)) if member.object.span() == ident.span => {
        Some(member.property.name.as_str())
      }
      _ => None,
    };
    let called = matches!(self.parent(0), Some(AstKind::CallExpression(call)) if call.callee.span() == ident.span);
    match resolved {
      None if ident.reference_id.get().is_some() => {
        if REFLECTIVE_GLOBALS.contains(&name) {
          self.bail_global(ident.span, &format!("`{name}` may reach the global object"));
        } else if name == "Symbol"
          && !called
          && !member.is_some_and(|m| ALLOWED_SYMBOL_MEMBERS.contains(&m))
        {
          self.bail_global(ident.span, "`Symbol` may reveal protocol symbols");
        } else if name == "Object" && !member.is_some_and(|m| ALLOWED_OBJECT_MEMBERS.contains(&m)) {
          self.bail_global(ident.span, "`Object` member may reveal protocol symbols");
        }
      }
      Some(Resolved::Namespace(target)) => {
        let Some(member_name) = member else {
          if self.shared.prefixes[target.index()].is_some() {
            self.bail_global(ident.span, "trusted module namespace used as a value");
          }
          return;
        };
        let resolved = self.shared.resolve_export(target, member_name);
        let member_span = self.parent(0).map(GetSpan::span).unwrap_or(ident.span);
        self.check_api(resolved, member_span, 1);
      }
      Some(Resolved::Symbol(_)) => self.check_api(resolved, ident.span, 0),
      None => {}
    }
  }

  /// Untrusted reference to a trusted export, whose use expression has parent `depth`.
  fn check_api(&mut self, resolved: Option<Resolved>, span: Span, depth: usize) {
    let Some(Resolved::Symbol(symbol)) = resolved else { return };
    if self.shared.keys.contains_key(&symbol) {
      return;
    }
    let Some(name) = self.shared.api_name(resolved).map(str::to_string) else { return };
    let prefix = self.shared.prefixes[symbol.owner.index()].as_deref().unwrap_or_default();
    let call = match self.parent(depth) {
      Some(AstKind::CallExpression(call)) if call.callee.span() == span => Some(call),
      _ => None,
    };
    if name == "Effect.runFork"
      && call.is_some_and(|call| call.arguments.len() == 1)
      && matches!(self.parent(depth + 1), Some(AstKind::ExpressionStatement(_)))
    {
      return;
    }
    if RUN_APIS.contains(&name.as_str()) && call.is_some_and(|call| call.arguments.len() == 1) {
      return;
    }
    if ALLOWED_APIS.contains(&name.as_str()) || ALLOWED_MODULES.contains(&prefix) {
      return;
    }
    self.bail_global(span, &format!("`{name}` is not an audited API"));
  }

  fn classify_key_reference(&mut self, key: &str, span: Span, use_span: Span) {
    if !self.classified.contains(&use_span.start) {
      let parent = match self.parent(usize::from(use_span != span)) {
        Some(kind) => format!("{kind:?}").split('(').next().unwrap_or_default().to_string(),
        None => "program".to_string(),
      };
      self.bail_key(key, use_span, &format!("service key used in {parent}"));
    }
  }
}

impl<'a> VisitJs<'a> for UseCollector<'_, '_, 'a> {
  fn enter_node(&mut self, kind: AstKind<'a>) {
    self.path.push(kind);
  }

  fn leave_node(&mut self, _kind: AstKind<'a>) {
    self.path.pop();
  }

  fn visit_statements(&mut self, it: &ArenaVec<'a, Statement<'a>>) {
    let is_program = matches!(self.path.last(), Some(AstKind::Program(_)));
    for (index, statement) in it.iter().enumerate() {
      if is_program {
        self.top_stmt = index + 1;
      }
      self.visit_statement(statement);
    }
  }

  fn visit_yield_expression(&mut self, it: &YieldExpression<'a>) {
    if let (true, Some(argument)) = (it.delegate, &it.argument) {
      if let Some(key) = self.shared.key_of(self.module, argument).cloned() {
        self.classified.insert(strip(argument).span().start);
        let in_effect = self.is_effect_generator(0);
        // Skip parentheses around the `yield*`.
        let mut depth = 0;
        let mut use_span = it.span;
        while let Some(AstKind::ParenthesizedExpression(paren)) = self.parent(depth) {
          use_span = paren.span;
          depth += 1;
        }
        let target = match self.parent(depth) {
          // `(yield* K).name`, e.g. after Rolldown inlined a single-use const.
          Some(AstKind::StaticMemberExpression(member)) if member.object.span() == use_span => {
            (!self.is_write_target(depth + 1))
              .then(|| ConsumerKind::Names(vec![member.property.name.as_str().to_string()]))
          }
          Some(AstKind::VariableDeclarator(declarator))
            if matches!(self.parent(depth + 1), Some(AstKind::VariableDeclaration(d)) if d.kind == VariableDeclarationKind::Const)
              && declarator.init.as_ref().is_some_and(|init| strip(init).span() == it.span) =>
          {
            match &declarator.id {
              BindingPattern::BindingIdentifier(id) => {
                id.symbol_id.get().map(|symbol| ConsumerKind::Binding((self.module, symbol).into()))
              }
              BindingPattern::ObjectPattern(pattern) if pattern.rest.is_none() => pattern
                .properties
                .iter()
                .map(|property| {
                  if property.computed {
                    None
                  } else {
                    property
                      .key
                      .static_name()
                      .and_then(|n| n.as_js_str().as_str().map(String::from))
                  }
                })
                .collect::<Option<Vec<_>>>()
                .map(ConsumerKind::Names),
              _ => None,
            }
          }
          _ => None,
        };
        match (in_effect, target) {
          (true, Some(kind)) => {
            if let ConsumerKind::Names(names) = &kind {
              if names.iter().any(|name| INHERITED_MEMBERS.contains(&name.as_str())) {
                self.bail_key(&key, it.span, "inherited member destructured");
              }
            }
            self.consumers.push(Consumer { key, kind, chain: self.guards.clone() });
          }
          (false, _) => self.bail_key(&key, it.span, "`yield*` outside an Effect generator"),
          (true, None) => {
            self.bail_key(
              &key,
              it.span,
              "`yield*` result used other than by a const or a static read",
            );
          }
        }
      }
    }
    walk_js::walk_yield_expression(self, it);
  }

  fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
    let callee = strip(&it.callee);
    // `Key.of(value)`.
    if let Expression::StaticMemberExpression(member) = callee {
      if member.property.name == "of" && self.shared.key_of(self.module, &member.object).is_some() {
        self.classified.insert(strip(&member.object).span().start);
      }
    }
    let api = self.shared.api_of(self.module, callee);
    if let Some(kind @ ("Layer.succeed" | "Layer.effect")) = api {
      let key = it.arguments.first().and_then(Argument::as_expression).and_then(|first| {
        self.shared.key_of(self.module, first).cloned().map(|key| (key, strip(first).span()))
      });
      if let Some((key, key_span)) = key {
        self.classified.insert(key_span.start);
        if it.arguments.len() == 2 {
          let container = self.container_from(it.span, self.path.len() - 1);
          if let Some(value) = it.arguments[1].as_expression() {
            self.provider(&key, kind, value, container, it.span);
          }
        } else if let Some(AstKind::CallExpression(outer)) = self.parent(0) {
          if outer.callee.span() == it.span && outer.arguments.len() == 1 {
            let container = self.container_from(outer.span, self.path.len() - 2);
            if let Some(value) = outer.arguments[0].as_expression() {
              self.provider(&key, kind, value, container, outer.span);
            }
          } else {
            self.bail_key(&key, it.span, "curried provider not applied");
          }
        } else {
          self.bail_key(&key, it.span, "curried provider not applied");
        }
      }
    }
    if let Some("Context.Service" | "Context.Reference") = api {
      if !self.shared.key_creations.contains(&(self.module, it.callee.span().start)) {
        let is_curried_inner = matches!(self.parent(0), Some(AstKind::CallExpression(outer)) if outer.callee.span() == it.span);
        let creation = if is_curried_inner {
          match self.parent(0) {
            Some(AstKind::CallExpression(outer)) => match outer.arguments.first() {
              Some(Argument::StringLiteral(literal)) => literal.value.as_str().map(String::from),
              _ => None,
            },
            _ => None,
          }
        } else {
          match it.arguments.first() {
            Some(Argument::StringLiteral(literal)) => literal.value.as_str().map(String::from),
            _ => None,
          }
        };
        let trusted = self.trusted;
        match creation {
          Some(key) => self.bail_key(&key, it.span, "service key created outside a declaration"),
          // Effect's own modules create keys from constants or arguments.
          None if trusted => {}
          None => self.bail_global(it.span, "service key with a dynamic key string"),
        }
      }
    }
    walk_js::walk_call_expression(self, it);
  }

  fn visit_object_expression(&mut self, it: &ObjectExpression<'a>) {
    let Some(&site) = self.site_spans.get(&it.span.start) else {
      walk_js::walk_object_expression(self, it);
      return;
    };
    let kind = AstKind::ObjectExpression(self.alloc(it));
    self.enter_node(kind);
    for (index, property) in it.properties.iter().enumerate() {
      match property {
        ObjectPropertyKind::SpreadProperty(spread) => {
          // Spread-in methods may read any property through `this`.
          self.sites[site].top = true;
          self.sites[site].props.push(Prop { name: None, span: spread.span, prunable: false });
          self.visit_spread_element(spread);
        }
        ObjectPropertyKind::ObjectProperty(property) => {
          let name = if property.computed {
            None
          } else {
            property.key.static_name().and_then(|n| n.as_js_str().as_str().map(String::from))
          };
          let value = strip(&property.value);
          let (pure, this_risk) = match value {
            Expression::ArrowFunctionExpression(_)
            | Expression::StringLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_) => (true, false),
            Expression::TemplateLiteral(template) => (template.expressions.is_empty(), false),
            Expression::FunctionExpression(function) => {
              (true, function.body.as_ref().is_none_or(|body| contains_this(body)))
            }
            Expression::Identifier(_) => {
              let resolved = self.shared.resolve_expr(self.module, value);
              let this_free = matches!(resolved, Some(Resolved::Symbol(symbol)) if self.shared.this_free.contains(&symbol));
              // A local bound to a resolvable function or value.
              (
                true,
                !this_free
                  && !matches!(resolved, Some(Resolved::Symbol(s)) if self.is_const_value(s)),
              )
            }
            _ => (false, true),
          };
          let prunable = pure
            && property.kind == PropertyKind::Init
            && name.as_deref().is_some_and(|name| name != "__proto__");
          if this_risk || property.kind != PropertyKind::Init {
            self.sites[site].top = true;
          }
          self.sites[site].props.push(Prop { name, span: property.span, prunable });
          self.guards.push(Guard::Property(site, index));
          self.visit_object_property(property);
          self.guards.pop();
        }
      }
    }
    self.leave_node(kind);
  }

  fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
    // A constructor-local `const f = () => ...` is a guard: reads inside it
    // only count while something live references `f`.
    let kind = match self.parent(0) {
      Some(AstKind::VariableDeclaration(declaration)) => declaration.kind,
      _ => VariableDeclarationKind::Var,
    };
    let guard = match (&it.id, &it.init, kind) {
      (BindingPattern::BindingIdentifier(id), Some(init), VariableDeclarationKind::Const) => {
        let in_statement_list =
          matches!(self.parent(1), Some(AstKind::FunctionBody(_) | AstKind::BlockStatement(_)));
        let is_function = matches!(
          strip(init),
          Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
        );
        id.symbol_id.get().filter(|_| in_statement_list && is_function).map(|symbol| {
          let symbol_ref: SymbolRef = (self.module, symbol).into();
          self.guard_bindings.push(GuardBinding { symbol: symbol_ref, declarator: it.span });
          symbol_ref
        })
      }
      _ => None,
    };
    if let Some(symbol) = guard {
      let kind = AstKind::VariableDeclarator(self.alloc(it));
      self.enter_node(kind);
      self.visit_binding_pattern(&it.id);
      self.guards.push(Guard::Binding(symbol));
      if let Some(init) = &it.init {
        self.visit_expression(init);
      }
      self.guards.pop();
      self.leave_node(kind);
    } else {
      walk_js::walk_variable_declarator(self, it);
    }
  }

  fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
    self.record_reference(it);
    let resolved = self.shared.resolve_ident(self.module, it);
    if !self.trusted {
      self.check_untrusted_reference(it, resolved);
    }
    // The use expression of a key: the identifier, or `ns.Key`.
    let (key_symbol, use_span) = match resolved {
      Some(Resolved::Symbol(symbol)) => (Some(symbol), it.span),
      Some(Resolved::Namespace(target)) => match self.parent(0) {
        Some(AstKind::StaticMemberExpression(member)) if member.object.span() == it.span => {
          match self.shared.resolve_export(target, member.property.name.as_str()) {
            Some(Resolved::Symbol(symbol)) => (Some(symbol), member.span),
            _ => (None, it.span),
          }
        }
        _ => (None, it.span),
      },
      None => (None, it.span),
    };
    if let Some(key) = key_symbol.and_then(|symbol| self.shared.keys.get(&symbol)).cloned() {
      // Writes to a key's members can change what other keys resolve to.
      let member_written = match self.parent(usize::from(use_span != it.span)) {
        Some(AstKind::StaticMemberExpression(member)) if member.object.span() == use_span => {
          self.is_write_target(1 + usize::from(use_span != it.span))
        }
        _ => false,
      };
      if member_written {
        self.bail_global(use_span, "service key member written");
      }
      self.classify_key_reference(&key, it.span, use_span);
    }
  }

  fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
    if !self.trusted && matches!(it.property.name.as_str(), "context" | "mapUnsafe") {
      let on_namespace =
        matches!(self.shared.resolve_expr(self.module, &it.object), Some(Resolved::Namespace(_)));
      if !on_namespace {
        self.bail_global(it.span, "member may read a whole Context");
      }
    }
    walk_js::walk_static_member_expression(self, it);
  }

  fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
    if !self.trusted {
      self.bail_global(it.span, "dynamic import");
    }
    walk_js::walk_import_expression(self, it);
  }
}

impl UseCollector<'_, '_, '_> {
  fn is_const_value(&self, symbol: SymbolRef) -> bool {
    let scoping = self.shared.scoping(symbol.owner);
    scoping.symbol_flags(symbol.symbol).is_const_variable()
      && self.shared.this_free.contains(&symbol)
  }
}

struct Pruner<'s> {
  properties: &'s FxHashSet<u32>,
  declarators: &'s FxHashSet<u32>,
}

impl<'a> VisitJsMut<'a> for Pruner<'_> {
  fn visit_object_expression(&mut self, it: &mut ObjectExpression<'a>) {
    it.properties.retain(|property| !self.properties.contains(&property.span().start));
    walk_js_mut::walk_object_expression(self, it);
  }

  fn visit_statements(&mut self, it: &mut ArenaVec<'a, Statement<'a>>) {
    for statement in it.iter_mut() {
      if let Statement::VariableDeclaration(declaration) = statement {
        declaration.declarations.retain(|d| !self.declarators.contains(&d.span.start));
      }
    }
    it.retain(|statement| {
      !matches!(statement, Statement::VariableDeclaration(declaration) if declaration.declarations.is_empty())
    });
    walk_js_mut::walk_statements(self, it);
  }
}

impl LinkStage<'_> {
  #[tracing::instrument(level = "debug", skip_all)]
  #[expect(clippy::too_many_lines, reason = "one pass: collection, fixpoint and edits")]
  pub(super) fn effect_service_shaking(&mut self) {
    if !self.options.experimental.is_effect_service_shaking_enabled() {
      return;
    }
    let mut report = Report::default();

    // == shared facts
    let mut namespaces = FxHashMap::default();
    let mut prefixes = vec![None; self.module_table.modules.len()];
    let mut resolved_exports = vec![FxHashMap::default(); self.module_table.modules.len()];
    for module in &self.module_table.modules {
      match module.as_normal() {
        Some(normal) => {
          namespaces
            .insert(self.symbols.canonical_ref_for(normal.namespace_object_ref), normal.idx);
          prefixes[normal.idx.index()] = trusted_prefix(normal.id.as_str());
          resolved_exports[normal.idx.index()] = self.metas[normal.idx]
            .resolved_exports
            .iter()
            .map(|(name, export)| (name.to_string(), export.symbol_ref))
            .collect();
        }
        None => {
          if let Some(external) = module.as_external() {
            let name = external.name.as_str();
            let builtin = name.starts_with("node:")
              || NODE_BUILTINS.contains(&name)
              || name.starts_with("bun:");
            if !builtin {
              report.global_bailouts.push(format!("external import `{name}`"));
            }
          }
        }
      }
    }
    for entry in &self.user_defined_entry_modules {
      let Some(module) = self.module_table[*entry].as_normal() else { continue };
      if !module.named_exports.is_empty() || module.has_star_export() {
        report.global_bailouts.push(format!(
          "entry module `{}` has exports, which code outside the bundle can call",
          module.stable_id
        ));
      }
    }
    let mut api_names = FxHashMap::default();
    for exports in &resolved_exports {
      for symbol in exports.values() {
        let canonical = self.symbols.canonical_ref_for(*symbol);
        if let Some(prefix) = &prefixes[canonical.owner.index()] {
          let name = self.symbols.local_db(canonical.owner).scoping().symbol_name(canonical.symbol);
          api_names.entry(canonical).or_insert_with(|| format!("{prefix}.{name}"));
        }
      }
    }
    let mut shared = Shared {
      symbols: &self.symbols,
      namespaces,
      resolved_exports,
      prefixes,
      api_names,
      keys: FxHashMap::default(),
      key_creations: FxHashSet::default(),
      this_free: FxHashSet::default(),
    };

    // == pass A: keys
    let normal_modules: Vec<ModuleIdx> =
      self.module_table.modules.iter().filter_map(|m| m.as_normal().map(|n| n.idx)).collect();
    let mut key_creations = vec![];
    let mut this_free = vec![];
    for module in &normal_modules {
      let Some(ast) = self.ast_table[*module].as_ref() else { continue };
      ast.program.with_dependent(|_owner, dep| {
        let mut collector = KeyCollector {
          shared: &shared,
          module: *module,
          keys: vec![],
          this_free: vec![],
          depth: 0,
          kind: VariableDeclarationKind::Var,
        };
        collector.visit_program(&dep.program);
        key_creations.extend(collector.keys.into_iter().map(|key| (*module, key)));
        this_free.extend(collector.this_free);
      });
    }
    for symbol in this_free {
      shared.this_free.insert(shared.symbols.canonical_ref_for(symbol));
    }
    let mut dynamic_keys = vec![];
    for (module, (symbol, key, callee_start)) in key_creations {
      shared.key_creations.insert((module, callee_start));
      match key {
        KeyCreation::Literal(key) => {
          shared.keys.insert(shared.symbols.canonical_ref_for(symbol), key);
        }
        KeyCreation::Dynamic => dynamic_keys.push(module),
      }
    }
    for module in dynamic_keys {
      if shared.prefixes[module.index()].is_none() {
        report.global_bailouts.push(format!("service key with a dynamic key string ({module:?})"));
      }
    }

    // == pass B: uses
    let mut sites: Vec<Site> = vec![];
    let mut consumers = vec![];
    let mut providers = vec![];
    let mut guard_bindings = vec![];
    let mut refs: FxHashMap<ModuleIdx, FxHashMap<ReferenceId, RefInfo>> = FxHashMap::default();
    let mut key_bailouts = vec![];
    let mut site_spans: FxHashMap<usize, (ModuleIdx, u32)> = FxHashMap::default();
    for module in &normal_modules {
      let Some(ast) = self.ast_table[*module].as_ref() else { continue };
      // Rolldown's own runtime module is trusted like the effect package.
      let trusted = shared.prefixes[module.index()].is_some() || *module == self.runtime.id();
      ast.program.with_dependent(|_owner, dep| {
        let mut collector = UseCollector {
          shared: &shared,
          module: *module,
          trusted,
          path: vec![],
          guards: vec![],
          top_stmt: 0,
          refs: FxHashMap::default(),
          classified: FxHashSet::default(),
          site_spans: FxHashMap::default(),
          sites: &mut sites,
          consumers: vec![],
          providers: vec![],
          guard_bindings: vec![],
          global_bailouts: vec![],
          key_bailouts: vec![],
        };
        collector.visit_program(&dep.program);
        for (start, index) in &collector.site_spans {
          site_spans.insert(*index, (*module, *start));
        }
        consumers.extend(collector.consumers.into_iter().map(|c| (*module, c)));
        providers.extend(collector.providers.into_iter().map(|p| (*module, p)));
        guard_bindings.extend(collector.guard_bindings);
        report.global_bailouts.extend(collector.global_bailouts);
        key_bailouts.extend(collector.key_bailouts);
        refs.insert(*module, collector.refs);
      });
    }

    let mut demand: FxHashMap<String, Demand> = FxHashMap::default();
    for (key, reason) in key_bailouts {
      demand.entry(key.clone()).or_default().set_all();
      report.key_bailouts.push((key, reason));
    }

    // Containers of providers must stay inside the bundle.
    // Uses of top-level bindings from other modules, indexed once.
    let mut imported_refs: FxHashMap<SymbolRef, Vec<&RefInfo>> = FxHashMap::default();
    for (module, module_refs) in &refs {
      for info in module_refs.values() {
        let Some(local) = info.root_symbol else { continue };
        let canonical = shared.symbols.canonical_ref_for((*module, local).into());
        if canonical.owner != *module {
          imported_refs.entry(canonical).or_default().push(info);
        }
      }
    }
    let references_of = |symbol: SymbolRef| -> Vec<&RefInfo> {
      let scoping = shared.scoping(symbol.owner);
      let module_refs = refs.get(&symbol.owner);
      let mut out: Vec<&RefInfo> = scoping
        .get_resolved_reference_ids(symbol.symbol)
        .iter()
        .filter_map(|id| module_refs.and_then(|r| r.get(id)))
        .collect();
      if let Some(imported) = imported_refs.get(&symbol) {
        out.extend(imported.iter().copied());
      }
      out
    };
    for (module, (key, container)) in &providers {
      let mut pending = vec![*container];
      let mut seen = FxHashSet::default();
      while let Some(container) = pending.pop() {
        match container {
          Container::Terminal => {}
          Container::Escape => {
            demand.entry(key.clone()).or_default().set_all();
            report.key_bailouts.push((key.clone(), format!("Layer escapes (module {module:?})")));
            break;
          }
          Container::Binding(symbol) => {
            if !seen.insert(symbol) {
              continue;
            }
            for info in references_of(symbol) {
              pending.push(info.container);
            }
          }
        }
      }
    }

    // == liveness and demand fixpoint
    let mut site_bindings_reads: Vec<SiteBindingReads> = vec![];
    for (index, site) in sites.iter().enumerate() {
      let mut reads = vec![];
      let mut escaped = false;
      for binding in &site.bindings {
        for info in references_of(*binding) {
          match &info.member {
            Some((name, false)) if !INHERITED_MEMBERS.contains(&name.as_str()) => {
              reads.push((name.clone(), info.chain.clone()));
            }
            _ => escaped = true,
          }
        }
      }
      site_bindings_reads.push((index, reads, escaped));
    }
    let mut consumer_reads: Vec<(String, String, Vec<Guard>)> = vec![];
    for (module, consumer) in &consumers {
      match &consumer.kind {
        ConsumerKind::Names(names) => {
          for name in names {
            consumer_reads.push((consumer.key.clone(), name.clone(), consumer.chain.clone()));
          }
        }
        ConsumerKind::Binding(symbol) => {
          for info in references_of(*symbol) {
            match &info.member {
              Some((name, false)) if !INHERITED_MEMBERS.contains(&name.as_str()) => {
                let mut chain = consumer.chain.clone();
                chain.extend(info.chain.iter().copied());
                consumer_reads.push((consumer.key.clone(), name.clone(), chain));
              }
              Some((name, true)) => {
                demand.entry(consumer.key.clone()).or_default().set_all();
                report.key_bailouts.push((
                  consumer.key.clone(),
                  format!("service member `{name}` written (module {module:?})"),
                ));
              }
              _ => {
                demand.entry(consumer.key.clone()).or_default().set_all();
                report.key_bailouts.push((
                  consumer.key.clone(),
                  format!(
                    "service value used other than by static member reads (module {module:?} at {})",
                    info.span.start
                  ),
                ));
              }
            }
          }
        }
      }
    }
    let binding_refs: Vec<(SymbolRef, Vec<Vec<Guard>>)> = guard_bindings
      .iter()
      .map(|binding| {
        (
          binding.symbol,
          references_of(binding.symbol).iter().map(|info| info.chain.clone()).collect(),
        )
      })
      .collect();

    let mut site_demand: Vec<Demand> = sites.iter().map(|_| Demand::default()).collect();
    let mut live_bindings: FxHashSet<SymbolRef> = FxHashSet::default();
    let retained = |site_demand: &Vec<Demand>, site: usize, prop: usize| -> bool {
      let s = &sites[site];
      let Some(p) = s.props.get(prop) else { return true };
      s.top || !p.prunable || p.name.as_deref().is_none_or(|name| site_demand[site].has(name))
    };
    let live =
      |site_demand: &Vec<Demand>, live_bindings: &FxHashSet<SymbolRef>, chain: &[Guard]| {
        chain.iter().all(|guard| match guard {
          Guard::Property(site, prop) => retained(site_demand, *site, *prop),
          Guard::Binding(symbol) => live_bindings.contains(symbol),
        })
      };
    loop {
      let mut changed = false;
      for (key, name, chain) in &consumer_reads {
        if live(&site_demand, &live_bindings, chain) {
          changed |= demand.entry(key.clone()).or_default().add(name);
        }
      }
      for (index, site) in sites.iter().enumerate() {
        for key in &site.keys {
          if let Some(key_demand) = demand.get(key) {
            changed |= site_demand[index].add_all(key_demand);
          }
        }
      }
      for (index, reads, escaped) in &site_bindings_reads {
        if *escaped {
          changed |= site_demand[*index].set_all();
        }
        for (name, chain) in reads {
          if live(&site_demand, &live_bindings, chain) {
            changed |= site_demand[*index].add(name);
          }
        }
      }
      for (symbol, chains) in &binding_refs {
        if !live_bindings.contains(symbol)
          && chains.iter().any(|chain| live(&site_demand, &live_bindings, chain))
        {
          live_bindings.insert(*symbol);
          changed = true;
        }
      }
      if !changed {
        break;
      }
    }

    // == edits
    let mut removed_properties: FxHashMap<ModuleIdx, FxHashSet<u32>> = FxHashMap::default();
    let mut removed_spans: FxHashMap<ModuleIdx, Vec<Span>> = FxHashMap::default();
    let mut removed_declarators: FxHashMap<ModuleIdx, FxHashSet<u32>> = FxHashMap::default();
    if report.global_bailouts.is_empty() {
      for (index, site) in sites.iter().enumerate() {
        for (prop_index, prop) in site.props.iter().enumerate() {
          if retained(&site_demand, index, prop_index) {
            continue;
          }
          removed_properties.entry(site.module).or_default().insert(prop.span.start);
          removed_spans.entry(site.module).or_default().push(prop.span);
          report.removed.push(format!(
            "{}: property `{}`",
            self.module_table[site.module].stable_id(),
            prop.name.as_deref().unwrap_or_default()
          ));
        }
      }
      for binding in &guard_bindings {
        if live_bindings.contains(&binding.symbol) {
          continue;
        }
        let module = binding.symbol.owner;
        removed_declarators.entry(module).or_default().insert(binding.declarator.start);
        removed_spans.entry(module).or_default().push(binding.declarator);
        report.removed.push(format!(
          "{}: local `{}`",
          self.module_table[module].stable_id(),
          shared.scoping(module).symbol_name(binding.symbol.symbol)
        ));
      }
    }
    let _ = site_spans;

    // Statement references that only occurred inside removed code.
    let mut stmt_updates: Vec<(ModuleIdx, usize, FxHashSet<SymbolId>, Vec<Span>)> = vec![];
    for (module, spans) in &removed_spans {
      let within = |span: Span| spans.iter().any(|removed| removed.contains_inclusive(span));
      let mut counts: FxHashMap<(usize, SymbolId), (usize, usize)> = FxHashMap::default();
      for info in refs.get(module).into_iter().flat_map(|r| r.values()) {
        let Some(symbol) = info.root_symbol else { continue };
        let entry = counts.entry((info.top_stmt, symbol)).or_default();
        entry.0 += 1;
        if within(info.span) {
          entry.1 += 1;
        }
      }
      let mut by_stmt: FxHashMap<usize, FxHashSet<SymbolId>> = FxHashMap::default();
      for ((stmt, symbol), (total, removed)) in counts {
        if removed > 0 && removed == total {
          by_stmt.entry(stmt).or_default().insert(symbol);
        }
      }
      let touched: FxHashSet<usize> = refs
        .get(module)
        .into_iter()
        .flat_map(|r| r.values())
        .filter(|info| within(info.span))
        .map(|info| info.top_stmt)
        .collect();
      for stmt in touched {
        stmt_updates.push((
          *module,
          stmt,
          by_stmt.remove(&stmt).unwrap_or_default(),
          spans.clone(),
        ));
      }
    }
    drop(shared);

    for (module, properties) in &removed_properties {
      let declarators = removed_declarators.remove(module).unwrap_or_default();
      if let Some(ast) = self.ast_table[*module].as_mut() {
        ast.program.with_mut(|fields| {
          Pruner { properties, declarators: &declarators }.visit_program(fields.program);
        });
      }
    }
    for (module, declarators) in &removed_declarators {
      if let Some(ast) = self.ast_table[*module].as_mut() {
        let properties = FxHashSet::default();
        ast.program.with_mut(|fields| {
          Pruner { properties: &properties, declarators }.visit_program(fields.program);
        });
      }
    }
    for (module, stmt, symbols, spans) in stmt_updates {
      if stmt >= self.stmt_infos[module].len() {
        continue;
      }
      let stmt_info = self.stmt_infos[module].get_mut(StmtInfoIdx::new(stmt));
      stmt_info.referenced_symbols.retain(|reference| match reference {
        SymbolOrMemberExprRef::Symbol(symbol) => {
          !(symbol.owner == module && symbols.contains(&symbol.symbol))
        }
        SymbolOrMemberExprRef::MemberExpr(member) => {
          !spans.iter().any(|removed| removed.contains_inclusive(member.span))
        }
      });
    }

    tracing::debug!(?report, "effect service shaking");
    #[expect(clippy::print_stderr, reason = "opt-in report for measuring the experimental pass")]
    if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT").is_some() {
      eprintln!("[effect-service-shaking] global bailouts: {:#?}", report.global_bailouts);
      eprintln!("[effect-service-shaking] key bailouts: {:#?}", report.key_bailouts);
      eprintln!("[effect-service-shaking] removed: {:#?}", report.removed);
      let mut demand: Vec<_> = demand
        .iter()
        .map(|(key, d)| {
          let mut names: Vec<_> = d.names.iter().cloned().collect();
          names.sort();
          (key.clone(), if d.all { "all".to_string() } else { names.join(",") })
        })
        .collect();
      demand.sort();
      eprintln!("[effect-service-shaking] demand: {demand:#?}");
    }
  }
}
