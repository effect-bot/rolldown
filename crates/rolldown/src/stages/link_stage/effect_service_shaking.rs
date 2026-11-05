//! Experimental removal of Effect service methods that no consumer reads.
//!
//! Enabled with `experimental.effectServiceShaking`. The pass runs in the link
//! stage, after symbols are bound and before statements are included, and
//! works on the module ASTs with oxc's scoping and Rolldown's symbol database.
//!
//! Base case only:
//! - Service keys are module-level `const K = Context.Service(...)` declarations
//!   with a literal key string, or `class K extends Context.Service<..>()("key")`
//!   whose body never mentions `this`.
//! - Consumers are `const s = yield* K` (or `const { a } = yield* K`) inside an
//!   `Effect.gen` / `Effect.fn` generator, where `s` is only read with static
//!   member access.
//! - Providers are `Layer.succeed(K, {...})` and `Layer.effect(K, e)` where `e` is
//!   built from `Effect.gen`, `Effect.sync`, `Effect.succeed` and
//!   `Effect.acquireRelease`, and returns `K.of({...})` or `{...}`. Outside the
//!   trusted packages, `e` may also be `make(args)` for a module-level
//!   `Effect.fn` constructor returning the object built by a module-level
//!   function, when every reference to both is such a followed call.
//!
//! Demand is attached to the key string. Reads inside a removed method, or
//! inside a constructor-local function only that method used, do not count:
//! demand is a least fixpoint over code that can still run. Code that runs
//! while a provider is constructed always counts, and the pass never removes
//! statements other than unused local `const` functions, so acquisition,
//! release and construction-time calls keep running.
//!
//! Anything outside the base case keeps every method of the key involved.
//!
//! The pass relies on the POC contract (`docs/in-depth/effect-service-shaking.md`):
//! user code reaches services only through keyed lookups and does not reflect
//! on Effect runtime objects (A1), the program is closed (A2), Effect reads
//! services only by key (A3), and there is one bundled `effect` (A4). The
//! cheap syntactic breaches (code loading, entry exports, external imports,
//! string-keyed Context APIs, keyed lookups other than `yield*` by an untracked
//! key, builtin prototype writes) turn the pass off.

use oxc::{
  allocator::Vec as ArenaVec,
  ast::{
    AstKind,
    ast::{
      Argument, AssignmentTargetPropertyIdentifier, AssignmentTargetPropertyProperty,
      BinaryOperator, BindingPattern, BindingProperty, CallExpression, Class, ClassElement,
      ComputedMemberExpression, Declaration, Expression, Function, FunctionBody, FunctionType,
      IdentifierReference, ImportExpression, ObjectExpression, ObjectPropertyKind, PropertyKey,
      PropertyKind, Statement, StaticMemberExpression, UnaryOperator, VariableDeclarationKind,
      VariableDeclarator, YieldExpression,
    },
  },
  ast_visit::{VisitJs, VisitJsMut, walk_js, walk_js_mut},
  semantic::{ReferenceId, Scoping, SymbolFlags, SymbolId},
  span::{GetSpan, Span},
};
use rolldown_common::{
  GetLocalDb, Module, ModuleIdx, Specifier, StmtInfoIdx, SymbolOrMemberExprRef, SymbolRef,
  SymbolRefDb,
};
use rolldown_error::BuildDiagnostic;
use rustc_hash::{FxHashMap, FxHashSet};

use super::LinkStage;

// Under the POC contract (A1) user code reaches services only through keyed
// lookups. These cheap syntactic cases turn the pass off instead of relying on it.
//
// Context functions that read or build a service map by key string.
const CONTEXT_INTERNAL_APIS: &[&str] = &["Context.getOrUndefinedUnsafe", "Context.makeUnsafe"];
// Keyed lookups other than `yield*`: their key argument may be a key the pass
// does not track, such as `{ key: "app/Store" }`.
const KEYED_LOOKUPS: &[&str] = &[
  "Context.get",
  "Context.getUnsafe",
  "Context.getOption",
  "Context.getOrElse",
  "Context.getOrUndefined",
  "Context.pick",
  "Context.omit",
  "Effect.service",
  "Effect.serviceOption",
  "Effect.updateService",
  "Effect.updateServiceScoped",
  "Layer.updateService",
  "Stream.updateService",
  "Channel.updateService",
];
// Data-last pipeables of `Effect.fn` constructors that only change the context
// or tracing of the constructor, never handing its result to user code.
const CONTEXT_PIPEABLES: &[&str] =
  &["Effect.updateService", "Effect.provideService", "Effect.withSpan", "Effect.annotateLogs"];
// Layer combinators that may be applied with `.pipe(...)`, curried.
const PIPED_LAYER_COMBINATORS: &[&str] = &["Layer.provide", "Layer.provideMerge", "Layer.merge"];

/// Arguments of a keyed lookup or `provideService` call that are keys, by API
/// and argument count; `None` when the shape is not recognised.
fn key_arguments(api: &str, argc: usize) -> Option<Vec<usize>> {
  let index = match (api, argc) {
    ("Context.pick" | "Context.omit", _) => return Some((0..argc).collect()),
    ("Context.get" | "Context.getUnsafe" | "Context.getOption" | "Context.getOrUndefined", 2)
    | (
      "Context.getOrElse"
      | "Effect.updateService"
      | "Layer.updateService"
      | "Stream.updateService"
      | "Channel.updateService"
      | "Effect.provideService"
      | "Effect.provideServiceEffect",
      3,
    ) => 1,
    ("Context.get" | "Context.getUnsafe" | "Context.getOption" | "Context.getOrUndefined", 1)
    | (
      "Context.getOrElse"
      | "Effect.updateService"
      | "Layer.updateService"
      | "Stream.updateService"
      | "Channel.updateService"
      | "Effect.provideServiceEffect",
      2,
    )
    | ("Effect.provideService", 1 | 2)
    | ("Effect.updateServiceScoped" | "Effect.service" | "Effect.serviceOption", 1..) => 0,
    _ => return None,
  };
  Some(vec![index])
}

// Ways to run code that is not part of the module graph (A2), by global name,
// member name (`globalThis.eval`, `fn.constructor`, `process.getBuiltinModule`)
// or imported name.
const CODE_LOADERS: &[&str] = &[
  "eval",
  "Function",
  "require",
  "constructor",
  "createRequire",
  "getBuiltinModule",
  "compileFunction",
  "runInThisContext",
  "runInNewContext",
  "runInContext",
];
// Node modules whose operations run code in this realm (A2).
const CODE_LOADING_MODULES: &[&str] = &["vm", "module", "inspector", "repl"];
// Host values that lead to code loaders (`process.getBuiltinModule`, the global
// `eval`): usable only through constant member names, calls and `typeof`.
// Names of the global object; `globalThis.Reflect` is the global `Reflect`.
const GLOBAL_OBJECTS: &[&str] = &["globalThis", "global", "self", "window"];
const CAPABILITY_GLOBALS: &[&str] =
  &["process", "globalThis", "global", "self", "window", "module"];
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
// Node builtins. Importing one observes nothing; using a code-loading one
// (`CODE_LOADING_MODULES`) turns the pass off.
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

/// How a trusted module's exports are named: `Effect.map` for the effect
/// package, `@effect/platform-node/NodeRuntime.runMain` otherwise.
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

/// Where a chain of namespace members starting at an identifier ends.
enum NamespaceChain {
  /// At an exported binding; `span` is the whole member chain, which is the
  /// expression at ancestor `depth`.
  Export { symbol: SymbolRef, span: Span, depth: usize },
  /// A namespace object used as a value.
  Value(Span),
  /// A member that is not exported.
  Unresolved,
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
  /// Returned from a module-level function, whose calls are checked in turn.
  Factory(SymbolRef),
  Escape,
}

#[derive(Debug)]
struct RefInfo {
  module: ModuleIdx,
  span: Span,
  top_stmt: usize,
  chain: Vec<Guard>,
  /// Static member read on the reference: (name, is a write target).
  member: Option<(String, bool)>,
  container: Container,
  /// Where the result flows when the reference is called directly.
  call_result: Option<Container>,
  /// The root-scope symbol referenced, for statement bookkeeping.
  root_symbol: Option<SymbolId>,
}

#[derive(Debug)]
struct Prop {
  name: Option<String>,
  span: Span,
  prunable: bool,
  /// May read other properties through `this` when called as a method, so
  /// keeping it keeps every property.
  this_risk: bool,
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
  /// Constructor and builder functions the object is returned through. Every
  /// reference to them must be a call the pass followed into a provider.
  via: Vec<SymbolRef>,
}

/// A provider object literal found in an Effect expression: (module, span
/// start, locals bound to it before it reaches a Context, keep every property,
/// functions it is returned through).
type FoundSite = (ModuleIdx, u32, Vec<SymbolRef>, bool, Vec<SymbolRef>);

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
  global_bailouts: Vec<Bailout>,
  key_bailouts: Vec<(String, String)>,
  removed: Vec<String>,
}

/// A reason the pass was turned off, reported as an `EFFECT_SERVICE_SHAKING` warning.
#[derive(Debug, Clone)]
struct Bailout {
  reason: String,
  /// Module and span start of the site.
  site: Option<(ModuleIdx, u32)>,
}

impl Bailout {
  fn global(reason: String) -> Self {
    Bailout { reason, site: None }
  }

  fn at(module: ModuleIdx, offset: u32, reason: String) -> Self {
    Bailout { reason, site: Some((module, offset)) }
  }
}

/// A Node builtin's name without the `node:` scheme.
fn bare_module_name(name: &str) -> &str {
  name.strip_prefix("node:").unwrap_or(name)
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
  /// Bindings of code loaders imported from Node builtins, directly or through
  /// re-exports: the import binding, or the external namespace.
  code_loaders: FxHashSet<SymbolRef>,
  /// Namespace and default imports of `node:process`.
  capabilities: FxHashSet<SymbolRef>,
  /// `const` bindings holding a constant string.
  const_strings: FxHashMap<SymbolRef, String>,
  /// Static member names of class-form keys.
  key_statics: FxHashMap<SymbolRef, FxHashSet<String>>,
  /// Module-level functions that return only object literals: their module
  /// and the literals' span starts.
  builders: FxHashMap<SymbolRef, (ModuleIdx, Vec<u32>)>,
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

  /// The string a constant expression evaluates to: literals, templates and
  /// `+` over them and over `const` bindings holding one.
  fn const_string(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<String> {
    let mut out = String::new();
    for part in self.string_parts(module, expr)? {
      match part {
        StringPart::Literal(literal) => out.push_str(&literal),
        StringPart::Binding(symbol) => out.push_str(self.const_strings.get(&symbol)?),
      }
    }
    Some(out)
  }

  fn string_parts(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<Vec<StringPart>> {
    Some(match strip(expr) {
      Expression::StringLiteral(literal) => {
        vec![StringPart::Literal(literal.value.as_str()?.to_string())]
      }
      Expression::TemplateLiteral(template) => {
        let mut parts = vec![];
        for (index, quasi) in template.quasis.iter().enumerate() {
          parts.push(StringPart::Literal(quasi.value.cooked.as_ref()?.as_str()?.to_string()));
          if let Some(expression) = template.expressions.get(index) {
            parts.extend(self.string_parts(module, expression)?);
          }
        }
        parts
      }
      Expression::BinaryExpression(binary) if binary.operator == BinaryOperator::Addition => {
        let mut parts = self.string_parts(module, &binary.left)?;
        parts.extend(self.string_parts(module, &binary.right)?);
        parts
      }
      Expression::Identifier(ident) => {
        let reference_id = ident.reference_id.get()?;
        let symbol = self.scoping(module).get_reference(reference_id).symbol_id()?;
        vec![StringPart::Binding(self.symbols.canonical_ref_for((module, symbol).into()))]
      }
      _ => return None,
    })
  }

  fn key_of(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<&String> {
    match self.resolve_expr(module, expr)? {
      Resolved::Symbol(symbol) => self.keys.get(&symbol),
      Resolved::Namespace(_) => None,
    }
  }

  /// A key creation `Context.Service("key")` or `Context.Service<..>()("key")`,
  /// with the start of its callee: `None` when the expression does not create a key.
  fn key_creation(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<(KeyCreation, u32)> {
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
    let key = match key_string(call) {
      Some(key) => KeyCreation::Literal(key),
      None => KeyCreation::Dynamic,
    };
    Some((key, call.callee.span().start))
  }
}

enum StringPart {
  Literal(String),
  Binding(SymbolRef),
}

/// The literal key string of a key creation call, if it has one.
fn key_string(call: &CallExpression<'_>) -> Option<String> {
  match call.arguments.first() {
    Some(Argument::StringLiteral(literal)) => literal.value.as_str().map(String::from),
    _ => None,
  }
}

/// Whether a function may read its receiver: anywhere in its parameters
/// (defaults included) or body, except nested non-arrow functions. Any nested
/// class counts as a read.
fn reads_this(function: &Function<'_>) -> bool {
  struct Finder(bool);
  impl<'a> VisitJs<'a> for Finder {
    fn visit_this_expression(&mut self, _it: &oxc::ast::ast::ThisExpression) {
      self.0 = true;
    }
    fn visit_function(&mut self, _it: &Function<'a>, _flags: oxc::semantic::ScopeFlags) {}
    // Heritage expressions and computed member names run with the enclosing
    // receiver. Classes are not modelled, so any class counts as a read.
    fn visit_class(&mut self, _it: &oxc::ast::ast::Class<'a>) {
      self.0 = true;
    }
  }
  let mut finder = Finder(false);
  finder.visit_formal_parameters(&function.params);
  if let Some(body) = &function.body {
    finder.visit_function_body(body);
  } else {
    finder.0 = true;
  }
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

/// Whether a class body mentions `this` or `super` anywhere, nested functions
/// included.
fn class_body_reads_this(class: &Class<'_>) -> bool {
  struct Finder(bool);
  impl VisitJs<'_> for Finder {
    fn visit_this_expression(&mut self, _it: &oxc::ast::ast::ThisExpression) {
      self.0 = true;
    }
    fn visit_super(&mut self, _it: &oxc::ast::ast::Super) {
      self.0 = true;
    }
  }
  let mut finder = Finder(false);
  finder.visit_class_body(&class.body);
  finder.0
}

/// Span starts of the object literals a function body returns, when every
/// return is an object literal.
fn object_returns(body: &FunctionBody<'_>) -> Option<Vec<u32>> {
  let mut returns = Returns(vec![], true);
  returns.statements(&body.statements);
  if !returns.1 || returns.0.is_empty() {
    return None;
  }
  returns
    .0
    .iter()
    .map(|value| match strip(value) {
      Expression::ObjectExpression(object) => Some(object.span.start),
      _ => None,
    })
    .collect()
}

/// Span starts of the object literals a function expression returns, when it
/// is a plain function or arrow and every return is an object literal.
fn builder_returns(function: &Expression<'_>) -> Option<Vec<u32>> {
  match strip(function) {
    Expression::ArrowFunctionExpression(f) if !f.r#async => match f.get_expression() {
      Some(value) => match strip(value) {
        Expression::ObjectExpression(object) => Some(vec![object.span.start]),
        _ => None,
      },
      None => f.body.as_function_body().and_then(object_returns),
    },
    Expression::FunctionExpression(f) if !f.r#async && !f.generator => {
      f.body.as_ref().and_then(|body| object_returns(body))
    }
    _ => None,
  }
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
      // Returns inside loops, switches and labels are returns of the function too.
      Statement::ForStatement(stmt) => self.statement(&stmt.body),
      Statement::ForInStatement(stmt) => self.statement(&stmt.body),
      Statement::ForOfStatement(stmt) => self.statement(&stmt.body),
      Statement::WhileStatement(stmt) => self.statement(&stmt.body),
      Statement::DoWhileStatement(stmt) => self.statement(&stmt.body),
      Statement::LabeledStatement(stmt) => self.statement(&stmt.body),
      Statement::SwitchStatement(stmt) => {
        for case in &stmt.cases {
          self.statements(&case.consequent);
        }
      }
      Statement::WithStatement(_) => self.1 = false,
      _ => {}
    }
  }
}

/// Pass A: service keys and functions that never read `this`.
struct KeyCollector<'s, 'l> {
  shared: &'s Shared<'l>,
  module: ModuleIdx,
  keys: Vec<(SymbolRef, KeyCreation, u32)>,
  key_statics: Vec<(SymbolRef, FxHashSet<String>)>,
  builders: Vec<(SymbolRef, Vec<u32>)>,
  this_free: Vec<SymbolRef>,
  const_strings: Vec<(SymbolRef, Vec<StringPart>)>,
  depth: usize,
  kind: VariableDeclarationKind,
}

impl<'a> VisitJs<'a> for KeyCollector<'_, '_> {
  fn visit_function(&mut self, it: &Function<'a>, flags: oxc::semantic::ScopeFlags) {
    if let Some(id) = &it.id {
      if matches!(it.r#type, FunctionType::FunctionDeclaration) {
        // A reassigned declaration may hold any function at runtime.
        let scoping = self.shared.scoping(self.module);
        if let Some(symbol) = id.symbol_id.get().filter(|s| !scoping.symbol_is_mutated(*s)) {
          if !reads_this(it) {
            self.this_free.push((self.module, symbol).into());
          }
          if self.depth == 0 && !it.r#async && !it.generator {
            if let Some(starts) = it.body.as_ref().and_then(|body| object_returns(body)) {
              self.builders.push(((self.module, symbol).into(), starts));
            }
          }
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
          if let Some(parts) = self.shared.string_parts(self.module, init) {
            self.const_strings.push((symbol_ref, parts));
          }
          match strip(init) {
            Expression::ArrowFunctionExpression(_) => self.this_free.push(symbol_ref),
            Expression::FunctionExpression(function) if !reads_this(function) => {
              self.this_free.push(symbol_ref);
            }
            _ => {}
          }
          if self.depth == 0 {
            if let Some((key, callee_start)) = self.shared.key_creation(self.module, init) {
              self.keys.push((symbol_ref, key, callee_start));
            }
            if let Some(starts) = builder_returns(init) {
              self.builders.push((symbol_ref, starts));
            }
          }
        }
      }
    }
    walk_js::walk_variable_declarator(self, it);
  }

  /// A module-level `class K extends Context.Service<..>()("key") { ... }` is a
  /// key declaration when its body never mentions `this` or `super`: static
  /// members then reach the key only by name, which is checked like any use.
  fn visit_class(&mut self, it: &Class<'a>) {
    let super_class = it.heritage.as_ref().map(|heritage| &heritage.expression);
    if let (0, Some(id), Some(super_class)) = (self.depth, &it.id, super_class) {
      let scoping = self.shared.scoping(self.module);
      let symbol = id.symbol_id.get().filter(|symbol| !scoping.symbol_is_mutated(*symbol));
      if let (Some(symbol), Some((key, callee_start))) =
        (symbol, self.shared.key_creation(self.module, super_class))
      {
        if it.r#type == oxc::ast::ast::ClassType::ClassDeclaration && !class_body_reads_this(it) {
          let symbol_ref: SymbolRef = (self.module, symbol).into();
          let statics = it
            .body
            .body
            .iter()
            .filter_map(|element| match element {
              ClassElement::MethodDefinition(method) if method.r#static && !method.computed => {
                method.key.static_name().and_then(|n| n.as_js_str().as_str().map(String::from))
              }
              ClassElement::PropertyDefinition(property)
                if property.r#static && !property.computed =>
              {
                property.key.static_name().and_then(|n| n.as_js_str().as_str().map(String::from))
              }
              _ => None,
            })
            .collect();
          self.keys.push((symbol_ref, key, callee_start));
          self.key_statics.push((symbol_ref, statics));
        }
      }
    }
    self.depth += 1;
    walk_js::walk_class(self, it);
    self.depth -= 1;
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
  /// Uses of exported bindings through a module namespace, `ns.binding`.
  namespace_refs: Vec<(SymbolRef, RefInfo)>,
  /// Use-expression spans of key references already classified.
  classified: FxHashSet<u32>,
  /// Object literal (module, span start) recognised as provider sites -> site index.
  site_spans: &'s mut FxHashMap<(ModuleIdx, u32), usize>,
  sites: &'s mut Vec<Site>,
  /// Initializers of this module's module-level `const` bindings.
  module_consts: FxHashMap<SymbolId, &'a Expression<'a>>,
  /// Callee spans of constructor and builder calls followed into a provider.
  accounted: Vec<(ModuleIdx, u32)>,
  /// Callee span starts of keyed lookups whose keys were all resolved.
  handled_lookups: FxHashSet<u32>,
  consumers: Vec<Consumer>,
  providers: Vec<(String, Container)>,
  guard_bindings: Vec<GuardBinding>,
  global_bailouts: Vec<Bailout>,
  key_bailouts: Vec<(String, String)>,
}

impl<'a> UseCollector<'_, '_, 'a> {
  fn bail_global(&mut self, span: Span, reason: &str) {
    self.global_bailouts.push(Bailout::at(self.module, span.start, reason.to_string()));
  }

  /// A property read or written by `name`, `None` when the name is not constant.
  /// Only constant string names can spell a code loader, so numbers and
  /// well-known symbols need no case of their own.
  fn check_member_name(&mut self, name: Option<&str>, span: Span) {
    if let Some(name) = name.filter(|name| CODE_LOADERS.contains(name)) {
      self.bail_global(span, &format!("`{name}` may run code outside the bundle"));
    }
  }

  /// A destructured property key.
  fn check_property_key(&mut self, key: &PropertyKey<'a>, computed: bool, span: Span) {
    let name = if computed {
      key.as_expression().and_then(|expr| self.shared.const_string(self.module, expr))
    } else {
      key.static_name().and_then(|n| n.as_js_str().as_str().map(String::from))
    };
    self.check_destructured_name(name.as_deref(), span);
  }

  fn check_destructured_name(&mut self, name: Option<&str>, span: Span) {
    self.check_member_name(name, span);
    if name == Some("getOwnPropertyDescriptor") {
      self.bail_global(span, "`getOwnPropertyDescriptor` used as a value");
    }
  }

  /// `Reflect.get` and `getOwnPropertyDescriptor` read the property named by
  /// their second argument, so a constant loader name there is checked like a
  /// member name. `Reflect.get` is recognised on the global `Reflect`, bare or
  /// qualified (`globalThis.Reflect`). `getOwnPropertyDescriptor` is matched on
  /// any receiver: a conservative fallback that covers aliases of `Object` the
  /// pass cannot track, and also matches user methods with that name. Used
  /// other than by a call, or with a spread before the key, the name they will
  /// read is unknown.
  fn check_reflective_read(&mut self, object: &Expression<'a>, name: &str, span: Span) {
    if name != "getOwnPropertyDescriptor" && !(name == "get" && self.is_global_reflect(object)) {
      return;
    }
    let call = match self.parent(0) {
      Some(AstKind::CallExpression(call)) if call.callee.span() == span => Some((
        call.span,
        call
          .arguments
          .iter()
          .take(2)
          .any(|argument| matches!(argument, Argument::SpreadElement(_))),
        call
          .arguments
          .get(1)
          .and_then(Argument::as_expression)
          .and_then(|key| self.shared.const_string(self.module, key)),
      )),
      _ => None,
    };
    match call {
      Some((call_span, true, _)) => {
        self.bail_global(call_span, &format!("`{name}` called with a spread before the key"));
      }
      Some((call_span, false, key)) => self.check_member_name(key.as_deref(), call_span),
      None => self.bail_global(span, &format!("`{name}` used as a value")),
    }
  }

  /// The global `Reflect`: the unresolved identifier, or a constant member
  /// named `Reflect` on a global object (`globalThis.Reflect`).
  fn is_global_reflect(&self, expr: &Expression<'a>) -> bool {
    match strip(expr) {
      Expression::Identifier(ident) => ident.name == "Reflect" && self.is_global_value(expr),
      Expression::StaticMemberExpression(member) => {
        member.property.name == "Reflect" && self.on_global_object(&member.object)
      }
      Expression::ComputedMemberExpression(member) => {
        self.shared.const_string(self.module, &member.expression).as_deref() == Some("Reflect")
          && self.on_global_object(&member.object)
      }
      _ => false,
    }
  }

  /// Whether `expr` is the global object, such as `globalThis` or
  /// `globalThis.self`, through constant names.
  fn on_global_object(&self, expr: &Expression<'a>) -> bool {
    match strip(expr) {
      Expression::Identifier(ident) => {
        GLOBAL_OBJECTS.contains(&ident.name.as_str()) && self.is_global_value(expr)
      }
      Expression::StaticMemberExpression(member) => {
        GLOBAL_OBJECTS.contains(&member.property.name.as_str())
          && self.on_global_object(&member.object)
      }
      Expression::ComputedMemberExpression(member) => {
        self
          .shared
          .const_string(self.module, &member.expression)
          .is_some_and(|name| GLOBAL_OBJECTS.contains(&name.as_str()))
          && self.on_global_object(&member.object)
      }
      _ => false,
    }
  }

  /// The global `Reflect` other than as the object of a member read or a
  /// `typeof` operand, such as `const { get } = Reflect`, hides which name
  /// `Reflect.get` will read.
  fn check_reflect_value(&mut self, span: Span) {
    let allowed = match self.parent(0) {
      Some(AstKind::StaticMemberExpression(member)) => member.object.span() == span,
      Some(AstKind::ComputedMemberExpression(member)) => member.object.span() == span,
      Some(AstKind::UnaryExpression(unary)) => unary.operator == UnaryOperator::Typeof,
      _ => false,
    };
    if !allowed {
      self.bail_global(span, "`Reflect` used as a value");
    }
  }

  /// Code loaders and the host values that lead to them (A2).
  fn check_host_reference(&mut self, it: &IdentifierReference<'a>) {
    let local = it
      .reference_id
      .get()
      .and_then(|reference| self.shared.scoping(self.module).get_reference(reference).symbol_id())
      .map(|symbol| -> SymbolRef { (self.module, symbol).into() });
    let bindings = local.map(|local| (local, self.shared.symbols.canonical_ref_for(local)));
    let bound_to = |set: &FxHashSet<SymbolRef>| {
      bindings.is_some_and(|(local, canonical)| set.contains(&local) || set.contains(&canonical))
    };
    let typeof_operand = matches!(self.parent(0), Some(AstKind::UnaryExpression(unary))
      if unary.operator == UnaryOperator::Typeof);
    if bound_to(&self.shared.code_loaders) {
      if !typeof_operand {
        self.bail_global(it.span, "host operation that loads code");
      }
      return;
    }
    let global_capability = local.is_none()
      && it.reference_id.get().is_some()
      && CAPABILITY_GLOBALS.contains(&it.name.as_str());
    if !global_capability && !bound_to(&self.shared.capabilities) {
      return;
    }
    // Follow constant member names while they still lead to a capability
    // (`globalThis.process`); any other member is a plain read or call.
    let mut span = it.span;
    let mut depth = 0;
    loop {
      let name = match self.parent(depth) {
        Some(AstKind::StaticMemberExpression(member)) if member.object.span() == span => {
          Some((member.property.name.to_string(), member.span))
        }
        Some(AstKind::ComputedMemberExpression(member)) if member.object.span() == span => {
          match self.shared.const_string(self.module, &member.expression) {
            Some(name) => Some((name, member.span)),
            None => {
              self.bail_global(it.span, "host capability read by a non-constant name");
              return;
            }
          }
        }
        _ => None,
      };
      match name {
        Some((name, member_span)) if CAPABILITY_GLOBALS.contains(&name.as_str()) => {
          span = member_span;
          depth += 1;
        }
        Some(_) => return,
        None => break,
      }
    }
    let typeof_operand = matches!(self.parent(depth), Some(AstKind::UnaryExpression(unary))
      if unary.operator == UnaryOperator::Typeof);
    if !typeof_operand {
      self.bail_global(it.span, "host capability used as a value");
    }
  }

  /// Whether a provider property's identifier value is initialised wherever the
  /// provider object is built, so reading it cannot throw. Function declarations
  /// and `var` always are; a `const`, `let` or class must be declared earlier in
  /// the source with no hoisted function declaration between, which could run
  /// the site first, and not in another case of a switch the site is in, since
  /// jumping to a case skips the cases before it. An import may be
  /// uninitialised through a module cycle, even one back to its own module.
  fn initialized_at(&self, value: &Expression<'a>, site: u32) -> bool {
    let Expression::Identifier(ident) = value else { return false };
    let Some(reference) = ident.reference_id.get() else { return false };
    let scoping = self.shared.scoping(self.module);
    let Some(symbol) = scoping.get_reference(reference).symbol_id() else { return false };
    let flags = scoping.symbol_flags(symbol);
    if flags.contains(SymbolFlags::Import) {
      return false;
    }
    if flags.intersects(SymbolFlags::Function | SymbolFlags::FunctionScopedVariable) {
      return true;
    }
    let declared = scoping.symbol_span(symbol);
    declared.end <= site
      && self.path.iter().all(|kind| match kind {
        AstKind::Function(function) => {
          function.r#type != FunctionType::FunctionDeclaration
            || function.span.contains_inclusive(declared)
        }
        _ => true,
      })
      && self.path.windows(2).all(|pair| match pair {
        [AstKind::SwitchStatement(switch), AstKind::SwitchCase(case)] => {
          !switch.span.contains_inclusive(declared) || case.span.contains_inclusive(declared)
        }
        _ => true,
      })
  }

  /// An unresolved global, such as `Map`.
  fn is_global_value(&self, expr: &Expression<'_>) -> bool {
    matches!(strip(expr), Expression::Identifier(ident)
      if ident.reference_id.get().is_some() && self.shared.resolve_ident(self.module, ident).is_none())
  }

  /// Whether the member expression at `span` is read in place: the object of
  /// a further member read that is not written.
  fn read_in_place(&self, span: Span) -> bool {
    match self.parent(0) {
      Some(AstKind::StaticMemberExpression(member)) if member.object.span() == span => {
        !self.is_write_target(1)
      }
      Some(AstKind::ComputedMemberExpression(member)) if member.object.span() == span => {
        !self.is_write_target(1)
      }
      _ => false,
    }
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
        // `layer.pipe(Layer.provide(a), ...)` with curried Layer combinators.
        AstKind::StaticMemberExpression(member)
          if member.object.span() == current && member.property.name == "pipe" =>
        {
          let Some(AstKind::CallExpression(call)) =
            index.checked_sub(1).and_then(|i| self.path.get(i))
          else {
            return Container::Escape;
          };
          let combinators = !call.arguments.is_empty()
            && call.arguments.iter().all(|argument| {
              matches!(argument.as_expression().map(strip), Some(Expression::CallExpression(inner))
                if inner.arguments.len() == 1
                  && self.shared.api_of(self.module, &inner.callee)
                    .is_some_and(|api| PIPED_LAYER_COMBINATORS.contains(&api)))
            });
          if call.callee.span() != member.span || !combinators {
            return Container::Escape;
          }
          current = call.span;
          index -= 1;
        }
        // The body of `const f = (..) => value`, or `return value` directly in
        // a module-level function.
        AstKind::ArrowFunctionExpression(arrow)
          if arrow.get_expression().is_some_and(|body| body.span() == current) =>
        {
          return if arrow.r#async { Container::Escape } else { self.factory(arrow.span, index) };
        }
        AstKind::ReturnStatement(statement)
          if statement.argument.as_ref().is_some_and(|argument| argument.span() == current) =>
        {
          return match (
            index.checked_sub(1).and_then(|i| self.path.get(i)),
            index.checked_sub(2).and_then(|i| self.path.get(i)),
          ) {
            (Some(AstKind::FunctionBody(_)), Some(AstKind::ArrowFunctionExpression(arrow)))
              if !arrow.r#async =>
            {
              self.factory(arrow.span, index - 2)
            }
            (Some(AstKind::FunctionBody(_)), Some(AstKind::Function(function)))
              if !function.r#async && !function.generator =>
            {
              self.factory(function.span, index - 2)
            }
            _ => Container::Escape,
          };
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

  /// The function at ancestor `index`, with span `function`, as a container:
  /// a module-level `const` or function declaration, never reassigned, in an
  /// untrusted module.
  fn factory(&self, function: Span, index: usize) -> Container {
    // Only outside the trusted packages: A3 does not cover how Effect's own
    // modules read Effect's own services.
    if self.trusted {
      return Container::Escape;
    }
    let module_level = |i: usize| match self.path.get(i) {
      Some(AstKind::Program(_)) => true,
      Some(AstKind::ExportDeclaration(_)) => {
        matches!(i.checked_sub(1).and_then(|j| self.path.get(j)), Some(AstKind::Program(_)))
      }
      _ => false,
    };
    let scoping = self.shared.scoping(self.module);
    let symbol = match self.path.get(index) {
      Some(AstKind::Function(f)) if f.r#type == FunctionType::FunctionDeclaration => f
        .id
        .as_ref()
        .and_then(|id| id.symbol_id.get())
        .filter(|_| index.checked_sub(1).is_some_and(module_level)),
      _ => match (
        index.checked_sub(1).and_then(|i| self.path.get(i)),
        index.checked_sub(2).and_then(|i| self.path.get(i)),
      ) {
        (Some(AstKind::VariableDeclarator(declarator)), Some(AstKind::VariableDeclaration(d)))
          if d.kind == VariableDeclarationKind::Const
            && declarator.init.as_ref().is_some_and(|init| strip(init).span() == function)
            && index.checked_sub(3).is_some_and(module_level) =>
        {
          match &declarator.id {
            BindingPattern::BindingIdentifier(id) => id.symbol_id.get(),
            _ => None,
          }
        }
        _ => None,
      },
    };
    match symbol.filter(|symbol| !scoping.symbol_is_mutated(*symbol)) {
      Some(symbol) => {
        Container::Factory(self.shared.symbols.canonical_ref_for((self.module, symbol).into()))
      }
      None => Container::Escape,
    }
  }

  /// Where the result of calling the expression at `span`, whose parent is
  /// `self.path[index]`, flows: `None` when it is not called directly.
  fn call_result(&self, span: Span, index: usize) -> Option<Container> {
    match self.path.get(index) {
      Some(AstKind::CallExpression(call)) if call.callee.span() == span => {
        Some(index.checked_sub(1).map_or(Container::Escape, |i| self.container_from(call.span, i)))
      }
      _ => None,
    }
  }

  /// Provider objects reachable from an Effect expression in the base case.
  /// Returns false when some source is not a recognised object literal.
  fn effect_sites(&mut self, expr: &Expression<'a>, found: &mut Vec<FoundSite>) -> bool {
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
        // The release callback receives the acquired object. Only a single
        // plain parameter without a default is followed; anything else (more
        // parameters, defaults, a rest parameter, a pattern or `arguments`)
        // keeps the whole object.
        let (params, reads_args) = match args[1].as_expression().map(strip) {
          Some(Expression::ArrowFunctionExpression(f)) => (Some(&f.params), false),
          Some(Expression::FunctionExpression(f)) => {
            (Some(&f.params), f.body.as_ref().is_none_or(|body| reads_arguments(body)))
          }
          _ => (None, false),
        };
        let simple = |params: &oxc::ast::ast::FormalParameters<'a>| {
          params.rest.is_none()
            && params.items.len() <= 1
            && params.items.iter().all(|param| param.initializer.is_none())
        };
        let (binding, top): (Option<SymbolRef>, bool) = match params {
          Some(params) if simple(params) && !reads_args => match params.items.first() {
            None => (None, false),
            Some(param) => match &param.pattern {
              BindingPattern::BindingIdentifier(id) => {
                (id.symbol_id.get().map(|s| (self.module, s).into()), id.symbol_id.get().is_none())
              }
              _ => (None, true),
            },
          },
          _ => (None, true),
        };
        // Nested acquisitions keep the readers of every level.
        for (module, span, mut bindings, existing_top, via) in acquired {
          bindings.extend(binding);
          found.push((module, span, bindings, existing_top || top, via));
        }
        complete
      }
      // `make(args)`, where `make` is a module-level `Effect.fn` constructor
      // outside the trusted packages.
      _ => {
        if self.trusted {
          return false;
        }
        let Some(Resolved::Symbol(symbol)) = self.shared.resolve_expr(self.module, &call.callee)
        else {
          return false;
        };
        let Some(generator) = self.constructor_generator(symbol) else { return false };
        let mut returned = vec![];
        let complete = self.return_sites(generator, true, &mut returned);
        self.accounted.push((self.module, call.callee.span().start));
        for (module, span, bindings, top, mut via) in returned {
          via.push(symbol);
          found.push((module, span, bindings, top, via));
        }
        complete
      }
    }
  }

  /// The generator of a module-level `const make = Effect.fn(function* () {..}, ...)`
  /// in this module, when every pipeable only changes the constructor's context.
  fn constructor_generator(&self, symbol: SymbolRef) -> Option<&'a Expression<'a>> {
    if symbol.owner != self.module {
      return None;
    }
    let Expression::CallExpression(call) = strip(self.module_consts.get(&symbol.symbol)?) else {
      return None;
    };
    if !matches!(
      self.shared.api_of(self.module, &call.callee),
      Some("Effect.fn" | "Effect.fnUntraced")
    ) {
      return None;
    }
    let (first, pipeables) = call.arguments.split_first()?;
    let generator = first.as_expression()?;
    if !matches!(strip(generator), Expression::FunctionExpression(f) if f.generator) {
      return None;
    }
    pipeables.iter().all(|pipeable| self.is_context_pipeable(pipeable)).then_some(generator)
  }

  /// `Effect.updateService(K, f)` and similar, directly or through a
  /// module-level `const` of this module.
  fn is_context_pipeable(&self, pipeable: &Argument<'a>) -> bool {
    let Some(mut expr) = pipeable.as_expression() else { return false };
    if let Expression::Identifier(ident) = strip(expr) {
      let local = ident.reference_id.get().and_then(|reference| {
        self.shared.scoping(self.module).get_reference(reference).symbol_id()
      });
      match local.and_then(|symbol| self.module_consts.get(&symbol)) {
        Some(init) => expr = init,
        None => return false,
      }
    }
    matches!(strip(expr), Expression::CallExpression(call)
      if self.shared.api_of(self.module, &call.callee).is_some_and(|api| CONTEXT_PIPEABLES.contains(&api)))
  }

  fn return_sites(
    &mut self,
    function: &Expression<'a>,
    generator: bool,
    found: &mut Vec<FoundSite>,
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
      complete &= match strip(value) {
        // `return yield* effect` in an Effect generator returns its result.
        Expression::YieldExpression(yielded) if generator && yielded.delegate => {
          yielded.argument.as_ref().is_some_and(|effect| self.effect_sites(effect, found))
        }
        _ => self.value_sites(value, found),
      };
    }
    complete
  }

  fn value_sites(&mut self, value: &Expression<'a>, found: &mut Vec<FoundSite>) -> bool {
    match strip(value) {
      Expression::ObjectExpression(object) => {
        found.push((self.module, object.span.start, vec![], false, vec![]));
        true
      }
      Expression::CallExpression(call) => {
        // `build(args)`, where `build` is a module-level function returning object literals.
        let resolved =
          self.shared.resolve_expr(self.module, &call.callee).filter(|_| !self.trusted);
        if let Some(Resolved::Symbol(symbol)) = resolved {
          if let Some((module, starts)) = self.shared.builders.get(&symbol) {
            self.accounted.push((self.module, call.callee.span().start));
            for start in starts {
              found.push((*module, *start, vec![], false, vec![symbol]));
            }
            return true;
          }
        }
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
  /// Sources that are not recognised object literals are simply not sites, so
  /// whether every source was followed does not matter here.
  fn provider(&mut self, key: &str, kind: &str, value: &Expression<'a>, container: Container) {
    let mut found = vec![];
    if kind == "Layer.succeed" {
      self.value_sites(value, &mut found);
    } else {
      self.effect_sites(value, &mut found);
    }
    for (module, start, bindings, top, via) in found {
      let index = *self.site_spans.entry((module, start)).or_insert_with(|| {
        self.sites.push(Site {
          module,
          keys: FxHashSet::default(),
          props: vec![],
          top: false,
          bindings: vec![],
          via: vec![],
        });
        self.sites.len() - 1
      });
      let site = &mut self.sites[index];
      site.keys.insert(key.to_string());
      site.top |= top;
      site.bindings.extend(bindings);
      for symbol in via {
        if !site.via.contains(&symbol) {
          site.via.push(symbol);
        }
      }
    }
    self.providers.push((key.to_string(), container));
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
    let call_result = self.call_result(ident.span, self.path.len() - 1);
    // `ns.binding` is a use of the exported binding, from the member's position.
    if let Some(symbol) = symbol {
      let canonical = self.shared.symbols.canonical_ref_for((self.module, symbol).into());
      if let Resolved::Namespace(target) = self.shared.as_resolved(canonical) {
        if let NamespaceChain::Export { symbol: export, span, depth } =
          self.namespace_chain(ident.span, target)
        {
          let (container, call_result) = match self.path.len().checked_sub(2 + depth) {
            Some(index) => (self.container_from(span, index), self.call_result(span, index)),
            None => (Container::Escape, None),
          };
          self.namespace_refs.push((
            export,
            RefInfo {
              module: self.module,
              span,
              top_stmt: self.top_stmt,
              chain: self.guards.clone(),
              member: None,
              container,
              call_result,
              root_symbol: None,
            },
          ));
        }
      }
    }
    self.refs.insert(
      reference_id,
      RefInfo {
        module: self.module,
        span: ident.span,
        top_stmt: self.top_stmt,
        chain: self.guards.clone(),
        member,
        container,
        call_result,
        root_symbol,
      },
    );
  }

  /// Follows `ns.a.b...` from an identifier resolving to a namespace through
  /// every namespace hop.
  fn namespace_chain(&self, ident: Span, mut target: ModuleIdx) -> NamespaceChain {
    let mut current = ident;
    let mut depth = 0;
    loop {
      match self.parent(depth) {
        Some(AstKind::StaticMemberExpression(member)) if member.object.span() == current => {
          match self.shared.resolve_export(target, member.property.name.as_str()) {
            Some(Resolved::Symbol(symbol)) => {
              return NamespaceChain::Export { symbol, span: member.span, depth };
            }
            Some(Resolved::Namespace(next)) => {
              target = next;
              current = member.span;
              depth += 1;
            }
            None => return NamespaceChain::Unresolved,
          }
        }
        _ => return NamespaceChain::Value(current),
      }
    }
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
    match resolved {
      None if ident.reference_id.get().is_some() => match name {
        "eval" | "Function" | "require" => {
          self.bail_global(ident.span, &format!("`{name}` may run code outside the bundle"));
        }
        _ => {}
      },
      Some(Resolved::Namespace(target)) => match self.namespace_chain(ident.span, target) {
        NamespaceChain::Export { symbol, span, .. } => {
          self.check_api(Some(Resolved::Symbol(symbol)), span);
        }
        // Every export of the module, Layers included, may escape through it.
        NamespaceChain::Value(span) => self.bail_global(span, "module namespace used as a value"),
        NamespaceChain::Unresolved => {}
      },
      Some(Resolved::Symbol(_)) => self.check_api(resolved, ident.span),
      None => {}
    }
  }

  /// Untrusted reference to a trusted export.
  fn check_api(&mut self, resolved: Option<Resolved>, span: Span) {
    let Some(Resolved::Symbol(symbol)) = resolved else { return };
    if self.shared.keys.contains_key(&symbol) {
      return;
    }
    let Some(name) = self.shared.api_name(resolved).map(str::to_string) else { return };
    // Any other use, such as an alias, creates keys the pass cannot see, and
    // keys with the same string alias at runtime.
    if matches!(name.as_str(), "Context.Service" | "Context.Reference")
      && !self.shared.key_creations.contains(&(self.module, span.start))
    {
      self.bail_global(span, &format!("`{name}` used other than to declare a service key"));
    } else if CONTEXT_INTERNAL_APIS.contains(&name.as_str()) {
      self.bail_global(span, &format!("`{name}` reads Context internals"));
    } else if KEYED_LOOKUPS.contains(&name.as_str()) && !self.handled_lookups.contains(&span.start)
    {
      // A lookup the pass does not model, possibly by a key it does not track.
      self.bail_global(span, &format!("`{name}` looks a service up other than by `yield*`"));
    }
  }

  fn classify_key_reference(&mut self, key: &str, use_span: Span, use_depth: usize) {
    if !self.classified.contains(&use_span.start) {
      let parent = match self.parent(use_depth) {
        Some(kind) => format!("{kind:?}").split('(').next().unwrap_or_default().to_string(),
        None => "program".to_string(),
      };
      // Opaque code holding a key may change its key string, which moves
      // lookups to another service, so outside trusted modules this is global.
      if self.trusted {
        self.bail_key(key, use_span, &format!("service key used in {parent}"));
      } else {
        self.bail_global(use_span, &format!("service key `{key}` used in {parent}"));
      }
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
          // A non-Effect driver receives the key itself.
          (false, _) if self.trusted => {
            self.bail_key(&key, it.span, "`yield*` outside an Effect generator");
          }
          (false, _) => {
            self.bail_global(it.span, "service key yielded outside an Effect generator");
          }
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
    // A keyed lookup or `provideService` whose key arguments are all tracked
    // keys reaches only those services: each keeps every method. Otherwise a
    // lookup turns the pass off (`check_api`).
    if let Some(positions) = api.and_then(|api| key_arguments(api, it.arguments.len())) {
      let keys: Option<Vec<(String, u32)>> = positions
        .iter()
        .map(|&position| {
          let argument = it.arguments.get(position)?.as_expression()?;
          let key = self.shared.key_of(self.module, argument)?;
          Some((key.clone(), strip(argument).span().start))
        })
        .collect();
      if let (Some(api), Some(keys)) = (api.map(str::to_string), keys) {
        for (key, start) in keys {
          self.classified.insert(start);
          self.bail_key(&key, it.span, &format!("service handed to `{api}`"));
        }
        self.handled_lookups.insert(it.callee.span().start);
      }
    }
    if let Some(kind @ ("Layer.succeed" | "Layer.effect")) = api {
      let key = it.arguments.first().and_then(Argument::as_expression).and_then(|first| {
        self.shared.key_of(self.module, first).cloned().map(|key| (key, strip(first).span()))
      });
      if let Some((key, key_span)) = key {
        self.classified.insert(key_span.start);
        if it.arguments.len() == 2 {
          let container = self.container_from(it.span, self.path.len() - 1);
          if let Some(value) = it.arguments[1].as_expression() {
            self.provider(&key, kind, value, container);
          }
        } else if let Some(AstKind::CallExpression(outer)) = self.parent(0) {
          if outer.callee.span() == it.span && outer.arguments.len() == 1 {
            let container = self.container_from(outer.span, self.path.len() - 2);
            if let Some(value) = outer.arguments[0].as_expression() {
              self.provider(&key, kind, value, container);
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
        // The key string is on the outer call of the curried form `Context.Service<..>()("key")`.
        let creation_call = match self.parent(0) {
          Some(AstKind::CallExpression(outer)) if outer.callee.span() == it.span => outer,
          _ => it,
        };
        let trusted = self.trusted;
        match key_string(creation_call) {
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
    let Some(&site) = self.site_spans.get(&(self.module, it.span.start)) else {
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
          self.sites[site].props.push(Prop {
            name: None,
            span: spread.span,
            prunable: false,
            this_risk: false,
          });
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
            Expression::FunctionExpression(function) => (true, reads_this(function)),
            Expression::Identifier(_) => {
              // A binding known to hold a function that never reads `this`. Its
              // read stays unless it cannot throw (temporal dead zone).
              let resolved = self.shared.resolve_expr(self.module, value);
              let this_free = matches!(resolved, Some(Resolved::Symbol(symbol)) if self.shared.this_free.contains(&symbol));
              (self.initialized_at(value, it.span.start), !this_free)
            }
            _ => (false, true),
          };
          let prunable = pure
            && property.kind == PropertyKind::Init
            && name.as_deref().is_some_and(|name| name != "__proto__");
          // `__proto__: x` (non-computed, identifier or string key) sets the
          // prototype, whose inherited methods may read any own property
          // through `this`. Prototypes are not modelled, so keep the object.
          let sets_prototype = name.as_deref() == Some("__proto__");
          if sets_prototype || property.kind != PropertyKind::Init {
            self.sites[site].top = true;
          }
          // A value that may read `this` keeps every property only if it is
          // kept itself: a removed property cannot be called.
          self.sites[site].props.push(Prop { name, span: property.span, prunable, this_risk });
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
    let kind = AstKind::VariableDeclarator(self.alloc(it));
    self.enter_node(kind);
    self.visit_binding_pattern(&it.id);
    if let Some(symbol) = guard {
      self.guards.push(Guard::Binding(symbol));
    }
    if let Some(init) = &it.init {
      self.visit_expression(init);
    }
    if guard.is_some() {
      self.guards.pop();
    }
    self.leave_node(kind);
  }

  fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
    self.record_reference(it);
    let resolved = self.shared.resolve_ident(self.module, it);
    if !self.trusted {
      self.check_untrusted_reference(it, resolved);
      self.check_host_reference(it);
      if it.name == "Reflect" && it.reference_id.get().is_some() && resolved.is_none() {
        self.check_reflect_value(it.span);
      }
    }
    // The use expression of a key: the identifier, or `ns.Key` through any
    // number of namespace hops. `use_depth` is the ancestor index of its parent.
    let (key_symbol, use_span, use_depth) = match resolved {
      Some(Resolved::Symbol(symbol)) => (Some(symbol), it.span, 0),
      Some(Resolved::Namespace(target)) => match self.namespace_chain(it.span, target) {
        NamespaceChain::Export { symbol, span, depth } => (Some(symbol), span, depth + 1),
        NamespaceChain::Value(_) | NamespaceChain::Unresolved => (None, it.span, 0),
      },
      None => (None, it.span, 0),
    };
    if let Some(key) = key_symbol.and_then(|symbol| self.shared.keys.get(&symbol)).cloned() {
      // Writes to a key's members can change what other keys resolve to.
      let (member_written, static_read) = match self.parent(use_depth) {
        Some(AstKind::StaticMemberExpression(member)) if member.object.span() == use_span => {
          let written = self.is_write_target(use_depth + 1);
          // A static member a class-form key declares itself; its body never
          // mentions `this`, so the value reaches the key only by name.
          let declared = key_symbol
            .and_then(|symbol| self.shared.key_statics.get(&symbol))
            .is_some_and(|statics| statics.contains(member.property.name.as_str()));
          (written, declared && !written)
        }
        _ => (false, false),
      };
      if member_written {
        self.bail_global(use_span, "service key member written");
      }
      if static_read {
        self.classified.insert(use_span.start);
      }
      self.classify_key_reference(&key, use_span, use_depth);
    }
  }

  fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
    if !self.trusted
      && !matches!(self.shared.resolve_expr(self.module, &it.object), Some(Resolved::Namespace(_)))
    {
      self.check_member_name(Some(it.property.name.as_str()), it.span);
      self.check_reflective_read(&it.object, it.property.name.as_str(), it.span);
      if it.property.name == "Reflect" && self.on_global_object(&it.object) {
        self.check_reflect_value(it.span);
      }
      // A1 assumes builtins are not modified; writes to their prototypes are
      // cheap to see, so they turn the pass off instead.
      if it.property.name == "prototype"
        && self.is_global_value(&it.object)
        && !self.read_in_place(it.span)
      {
        self.bail_global(it.span, "builtin prototype modified or used as a value");
      }
    }
    walk_js::walk_static_member_expression(self, it);
  }

  fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
    if !self.trusted {
      let name = self.shared.const_string(self.module, &it.expression);
      self.check_member_name(name.as_deref(), it.span);
      if let Some(name) = name {
        self.check_reflective_read(&it.object, &name, it.span);
        if name == "Reflect" && self.on_global_object(&it.object) {
          self.check_reflect_value(it.span);
        }
      }
    }
    walk_js::walk_computed_member_expression(self, it);
  }

  fn visit_binding_property(&mut self, it: &BindingProperty<'a>) {
    if !self.trusted {
      self.check_property_key(&it.key, it.computed, it.span);
    }
    walk_js::walk_binding_property(self, it);
  }

  fn visit_assignment_target_property_property(
    &mut self,
    it: &AssignmentTargetPropertyProperty<'a>,
  ) {
    if !self.trusted {
      self.check_property_key(&it.name, it.computed, it.span);
    }
    walk_js::walk_assignment_target_property_property(self, it);
  }

  fn visit_assignment_target_property_identifier(
    &mut self,
    it: &AssignmentTargetPropertyIdentifier<'a>,
  ) {
    if !self.trusted {
      self.check_destructured_name(Some(it.binding.name.as_str()), it.span);
    }
    walk_js::walk_assignment_target_property_identifier(self, it);
  }

  fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
    if !self.trusted {
      self.bail_global(it.span, "dynamic import");
    }
    walk_js::walk_import_expression(self, it);
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
      if let Some(normal) = module.as_normal() {
        namespaces.insert(self.symbols.canonical_ref_for(normal.namespace_object_ref), normal.idx);
        prefixes[normal.idx.index()] = trusted_prefix(normal.id.as_str());
        resolved_exports[normal.idx.index()] = self.metas[normal.idx]
          .resolved_exports
          .iter()
          .map(|(name, export)| (name.to_string(), export.symbol_ref))
          .collect();
      }
    }
    // External imports of untrusted code. Rolldown's own runtime imports
    // `node:module` on Node, which is trusted like the effect package.
    let mut externals: FxHashSet<&str> = FxHashSet::default();
    for module in &self.module_table.modules {
      let Some(normal) = module.as_normal() else { continue };
      if prefixes[normal.idx.index()].is_some() || normal.idx == self.runtime.id() {
        continue;
      }
      for record in &normal.import_records {
        let Some(target) = record.resolved_module else { continue };
        if let Some(external) = self.module_table[target].as_external() {
          externals.insert(external.name.as_str());
        }
      }
    }
    for name in externals {
      let builtin = NODE_BUILTINS.contains(&bare_module_name(name)) || name.starts_with("bun:");
      if !builtin {
        report.global_bailouts.push(Bailout::global(format!("external import `{name}`")));
      }
    }
    for entry in &self.user_defined_entry_modules {
      let Some(module) = self.module_table[*entry].as_normal() else { continue };
      if !module.named_exports.is_empty() || module.has_star_export() {
        report.global_bailouts.push(Bailout::global(format!(
          "entry module `{}` has exports, which code outside the bundle can call",
          module.stable_id
        )));
      }
    }
    // Imports of Node builtins that load code, followed through re-exports by
    // canonical binding (A2).
    let mut code_loaders: FxHashSet<SymbolRef> = FxHashSet::default();
    let mut capabilities: FxHashSet<SymbolRef> = FxHashSet::default();
    for module in &self.module_table.modules {
      match module {
        Module::Normal(normal) => {
          for (symbol, named_import) in &normal.named_imports {
            let Some(target) = normal.import_records[named_import.record_idx].resolved_module
            else {
              continue;
            };
            let Some(external) = self.module_table[target].as_external() else { continue };
            let module_name = bare_module_name(external.name.as_str());
            let imported = match &named_import.imported {
              Specifier::Star => None,
              Specifier::Literal(name) => Some(name.as_str()),
            };
            if CODE_LOADING_MODULES.contains(&module_name)
              || imported.is_some_and(|name| CODE_LOADERS.contains(&name))
            {
              code_loaders.insert(*symbol);
            }
            if module_name == "process" && imported.is_none_or(|name| name == "default") {
              capabilities.insert(*symbol);
            }
          }
        }
        Module::External(external) => {
          let module_name = bare_module_name(external.name.as_str());
          if CODE_LOADING_MODULES.contains(&module_name) {
            code_loaders.insert(external.namespace_ref);
          }
          if module_name == "process" {
            capabilities.insert(external.namespace_ref);
          }
        }
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
      code_loaders,
      capabilities,
      const_strings: FxHashMap::default(),
      key_statics: FxHashMap::default(),
      builders: FxHashMap::default(),
    };

    // == pass A: keys
    let normal_modules: Vec<ModuleIdx> =
      self.module_table.modules.iter().filter_map(|m| m.as_normal().map(|n| n.idx)).collect();
    let mut key_creations = vec![];
    let mut key_statics = vec![];
    let mut builders = vec![];
    let mut this_free = vec![];
    let mut const_strings: Vec<(SymbolRef, Vec<StringPart>)> = vec![];
    for module in &normal_modules {
      let Some(ast) = self.ast_table[*module].as_ref() else { continue };
      ast.program.with_dependent(|_owner, dep| {
        let mut collector = KeyCollector {
          shared: &shared,
          module: *module,
          keys: vec![],
          key_statics: vec![],
          builders: vec![],
          this_free: vec![],
          const_strings: vec![],
          depth: 0,
          kind: VariableDeclarationKind::Var,
        };
        collector.visit_program(&dep.program);
        key_creations.extend(collector.keys.into_iter().map(|key| (*module, key)));
        key_statics.extend(collector.key_statics);
        builders.extend(collector.builders.into_iter().map(|builder| (*module, builder)));
        this_free.extend(collector.this_free);
        const_strings.extend(collector.const_strings);
      });
    }
    for symbol in this_free {
      shared.this_free.insert(shared.symbols.canonical_ref_for(symbol));
    }
    for (symbol, statics) in key_statics {
      shared.key_statics.insert(shared.symbols.canonical_ref_for(symbol), statics);
    }
    // Only outside the trusted packages: A3 does not cover how Effect's own
    // modules read Effect's own services.
    for (module, (symbol, starts)) in builders {
      if shared.prefixes[module.index()].is_none() {
        shared.builders.insert(shared.symbols.canonical_ref_for(symbol), (module, starts));
      }
    }
    // `const` strings may be built from other `const` strings, in any module order.
    let mut changed = true;
    while changed {
      changed = false;
      for (symbol, parts) in &const_strings {
        let symbol = shared.symbols.canonical_ref_for(*symbol);
        if shared.const_strings.contains_key(&symbol) {
          continue;
        }
        let value = parts.iter().try_fold(String::new(), |mut out, part| {
          match part {
            StringPart::Literal(literal) => out.push_str(literal),
            StringPart::Binding(binding) => out.push_str(shared.const_strings.get(binding)?),
          }
          Some(out)
        });
        if let Some(value) = value {
          shared.const_strings.insert(symbol, value);
          changed = true;
        }
      }
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
        report.global_bailouts.push(Bailout::at(
          module,
          0,
          "service key with a dynamic key string".to_string(),
        ));
      }
    }

    // == pass B: uses
    // Object literals returned by builder functions are visited as provider
    // sites in their own module, before any provider that calls them is seen.
    let mut sites: Vec<Site> = vec![];
    let mut site_spans: FxHashMap<(ModuleIdx, u32), usize> = FxHashMap::default();
    for (symbol, (module, starts)) in &shared.builders {
      for start in starts {
        site_spans.insert((*module, *start), sites.len());
        sites.push(Site {
          module: *module,
          keys: FxHashSet::default(),
          props: vec![],
          top: false,
          bindings: vec![],
          via: vec![*symbol],
        });
      }
    }
    let mut accounted: FxHashSet<(ModuleIdx, u32)> = FxHashSet::default();
    let mut consumers = vec![];
    let mut providers = vec![];
    let mut guard_bindings = vec![];
    let mut refs: FxHashMap<ModuleIdx, FxHashMap<ReferenceId, RefInfo>> = FxHashMap::default();
    let mut namespace_refs: Vec<(SymbolRef, RefInfo)> = vec![];
    let mut key_bailouts = vec![];
    for module in &normal_modules {
      let Some(ast) = self.ast_table[*module].as_ref() else { continue };
      // Rolldown's own runtime module is trusted like the effect package.
      let trusted = shared.prefixes[module.index()].is_some() || *module == self.runtime.id();
      ast.program.with_dependent(|_owner, dep| {
        let mut module_consts = FxHashMap::default();
        for statement in &dep.program.body {
          let declaration = match statement {
            Statement::VariableDeclaration(declaration) => declaration,
            Statement::ExportDeclaration(export) => match &export.declaration {
              Declaration::VariableDeclaration(declaration) => declaration,
              _ => continue,
            },
            _ => continue,
          };
          if declaration.kind != VariableDeclarationKind::Const {
            continue;
          }
          for declarator in &declaration.declarations {
            if let (BindingPattern::BindingIdentifier(id), Some(init)) =
              (&declarator.id, &declarator.init)
            {
              if let Some(symbol) = id.symbol_id.get() {
                module_consts.insert(symbol, init);
              }
            }
          }
        }
        let mut collector = UseCollector {
          shared: &shared,
          module: *module,
          trusted,
          path: vec![],
          guards: vec![],
          top_stmt: 0,
          refs: FxHashMap::default(),
          namespace_refs: vec![],
          classified: FxHashSet::default(),
          site_spans: &mut site_spans,
          sites: &mut sites,
          module_consts,
          accounted: vec![],
          handled_lookups: FxHashSet::default(),
          consumers: vec![],
          providers: vec![],
          guard_bindings: vec![],
          global_bailouts: vec![],
          key_bailouts: vec![],
        };
        collector.visit_program(&dep.program);
        accounted.extend(collector.accounted);
        consumers.extend(collector.consumers.into_iter().map(|c| (*module, c)));
        providers.extend(collector.providers.into_iter().map(|p| (*module, p)));
        guard_bindings.extend(collector.guard_bindings);
        report.global_bailouts.extend(collector.global_bailouts);
        key_bailouts.extend(collector.key_bailouts);
        refs.insert(*module, collector.refs);
        namespace_refs.extend(collector.namespace_refs);
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
    for (export, info) in &namespace_refs {
      imported_refs.entry(*export).or_default().push(info);
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
          // Every use of a function returning the Layer must be a call.
          Container::Factory(symbol) => {
            if !seen.insert(symbol) {
              continue;
            }
            for info in references_of(symbol) {
              pending.push(info.call_result.unwrap_or(Container::Escape));
            }
          }
        }
      }
    }

    // An object returned through constructor or builder functions is a site
    // only when it is provided and every reference to those functions is a
    // call the pass followed into a provider: another caller would get the
    // same pruned object.
    for site in &mut sites {
      let followed = |symbol: &SymbolRef| {
        references_of(*symbol)
          .iter()
          .all(|info| accounted.contains(&(info.module, info.span.start)))
      };
      if !site.via.is_empty() && (site.keys.is_empty() || !site.via.iter().all(followed)) {
        site.top = true;
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
      // A kept property that may read `this` keeps every property.
      for (index, site) in sites.iter().enumerate() {
        for (prop_index, prop) in site.props.iter().enumerate() {
          if prop.this_risk && retained(&site_demand, index, prop_index) {
            changed |= site_demand[index].set_all();
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

    let edited: FxHashSet<ModuleIdx> =
      removed_properties.keys().chain(removed_declarators.keys()).copied().collect();
    let empty = FxHashSet::default();
    for module in edited {
      let properties = removed_properties.get(&module).unwrap_or(&empty);
      let declarators = removed_declarators.get(&module).unwrap_or(&empty);
      if let Some(ast) = self.ast_table[module].as_mut() {
        ast.program.with_mut(|fields| {
          Pruner { properties, declarators }.visit_program(fields.program);
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

    self.report_effect_service_bailouts(&report.global_bailouts);
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

  /// Every reason the pass was turned off, one `EFFECT_SERVICE_SHAKING` warning
  /// per site, with the site's 1-based line and column.
  fn report_effect_service_bailouts(&mut self, bailouts: &[Bailout]) {
    let mut reported: FxHashSet<Option<(ModuleIdx, u32)>> = FxHashSet::default();
    for bailout in bailouts {
      if !reported.insert(bailout.site) {
        continue;
      }
      let site = bailout
        .site
        .and_then(|(module, offset)| self.module_table[module].as_normal().map(|n| (n, offset)));
      let (id, position) = match site {
        Some((normal, offset)) => {
          let before = normal.source.get(..offset as usize).unwrap_or_default();
          let line = before.matches('\n').count() + 1;
          let column = before.len() - before.rfind('\n').map_or(0, |newline| newline + 1) + 1;
          (Some(normal.id.to_string()), Some((line, column)))
        }
        None => (None, None),
      };
      self.diagnostics.push(
        BuildDiagnostic::effect_service_shaking(id, "bailout", bailout.reason.clone(), position)
          .with_severity_warning(),
      );
    }
  }
}
