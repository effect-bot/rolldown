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
//! Anything outside the base case keeps every method of the key involved.
//! Holding a Context or Fiber is fine. Services leave a Context through a
//! keyed lookup, which is tracked, or through an operation the pass treats as a
//! sink:
//! - reading a Context internal by name (`mapUnsafe`, `base`, `toJSON`, ...) or
//!   running code outside the module graph disables the pass;
//! - reflection, serialization and formatting disable it when their operand
//!   may hold an Effect runtime object, and a static read on such a value
//!   demands that name on every service (see "provenance" below).

use oxc::{
  allocator::Vec as ArenaVec,
  ast::{
    AstKind,
    ast::{
      Argument, ArrayExpressionElement, ArrowFunctionExpression, AssignmentExpression,
      AssignmentOperator, AssignmentTarget, AssignmentTargetPropertyIdentifier,
      AssignmentTargetPropertyProperty, AssignmentTargetWithDefault, BinaryExpression,
      BinaryOperator, BindingPattern, BindingProperty, CallExpression, CatchClause, ChainElement,
      Class, ComputedMemberExpression, Expression, ForInStatement, ForOfStatement,
      ForStatementLeft, FormalParameters, Function, FunctionType, IdentifierReference,
      ImportDeclaration, ImportExpression, NewExpression, ObjectExpression, ObjectPropertyKind,
      Program, PropertyKey, PropertyKind, SimpleAssignmentTarget, Statement,
      StaticMemberExpression, TemplateLiteral, ThrowStatement, UnaryOperator, UpdateExpression,
      VariableDeclaration, VariableDeclarationKind, VariableDeclarator, WithStatement,
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

// The only ways for user code to reach a provider object are keyed lookups,
// which the pass tracks, and the operations below, which it treats as sinks.
//
// Context internals hold every service of a Context by key string. Reading
// one by name reaches every service without a key, so it disables the pass in
// any mode. `toJSON` returns the services of a Context.
const CONTEXT_INTERNALS: &[&str] =
  &["mapUnsafe", "_flat", "base", "overlay", "cacheRoot", "toJSON"];
// Context functions that read or build a service map by key string.
const CONTEXT_INTERNAL_APIS: &[&str] = &["Context.getOrUndefinedUnsafe", "Context.makeUnsafe"];
// Accessor reflection on any object: these reach a getter such as `mapUnsafe`
// without reading it by name.
const REFLECTIVE_MEMBERS: &[&str] =
  &["__lookupGetter__", "__lookupSetter__", "__defineGetter__", "__defineSetter__"];
// Ways to run code that is not part of the module graph, by global name or
// member name (`globalThis.eval`, `fn.constructor`), in any mode.
const CODE_LOADERS: &[&str] = &["eval", "Function", "require", "constructor"];
const GLOBAL_OBJECTS: &[&str] =
  &["globalThis", "global", "self", "window", "frames", "parent", "top"];
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
// Node builtins other than those that run code in this realm (`vm`, `module`,
// `inspector`, `repl`).
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
  /// Start of the object literal.
  start: u32,
  keys: FxHashSet<String>,
  props: Vec<Prop>,
  /// Keep every property, e.g. a method may read `this`.
  top: bool,
  /// Locals bound to the provider object before it reaches a Context, such as
  /// an `acquireRelease` release callback parameter.
  bindings: Vec<SymbolRef>,
}

/// A provider object literal found in an Effect expression: (span start,
/// locals bound to it before it reaches a Context, keep every property).
type FoundSite = (u32, Vec<SymbolRef>, bool);

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
  /// `const` bindings holding a constant string.
  const_strings: FxHashMap<SymbolRef, String>,
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

  /// The property name a computed key evaluates to, if it is constant. Numbers
  /// and well-known symbols never name a Context internal.
  fn member_key(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<String> {
    if is_numeric(expr) {
      return Some(String::new());
    }
    if let Expression::StaticMemberExpression(member) = strip(expr) {
      if matches!(&member.object, Expression::Identifier(ident) if ident.name == "Symbol" && self.resolve_ident(module, ident).is_none())
        && matches!(member.property.name.as_str(), "iterator" | "asyncIterator")
      {
        return Some(String::new());
      }
    }
    self.const_string(module, expr)
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

/// Whether an expression always evaluates to a number.
fn is_numeric(expr: &Expression<'_>) -> bool {
  match strip(expr) {
    Expression::NumericLiteral(_) | Expression::UpdateExpression(_) => true,
    Expression::UnaryExpression(unary) => {
      matches!(
        unary.operator,
        UnaryOperator::UnaryNegation | UnaryOperator::UnaryPlus | UnaryOperator::BitwiseNot
      ) && is_numeric(&unary.argument)
    }
    // Every arithmetic and bitwise operator but `+` yields a number or BigInt.
    Expression::BinaryExpression(binary) => matches!(
      binary.operator,
      BinaryOperator::Subtraction
        | BinaryOperator::Multiplication
        | BinaryOperator::Division
        | BinaryOperator::Remainder
        | BinaryOperator::Exponential
        | BinaryOperator::ShiftLeft
        | BinaryOperator::ShiftRight
        | BinaryOperator::ShiftRightZeroFill
        | BinaryOperator::BitwiseOR
        | BinaryOperator::BitwiseXOR
        | BinaryOperator::BitwiseAnd
    ),
    _ => false,
  }
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
  const_strings: Vec<(SymbolRef, Vec<StringPart>)>,
  depth: usize,
  kind: VariableDeclarationKind,
}

impl<'a> VisitJs<'a> for KeyCollector<'_, '_> {
  fn visit_function(&mut self, it: &Function<'a>, flags: oxc::semantic::ScopeFlags) {
    if let Some(id) = &it.id {
      if matches!(it.r#type, FunctionType::FunctionDeclaration) && !reads_this(it) {
        // A reassigned declaration may hold any function at runtime.
        let scoping = self.shared.scoping(self.module);
        if let Some(symbol) = id.symbol_id.get().filter(|s| !scoping.symbol_is_mutated(*s)) {
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
  /// Uses of exported bindings through a module namespace, `ns.binding`.
  namespace_refs: Vec<(SymbolRef, RefInfo)>,
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
  /// Static reads in untrusted code: (receiver span, name, guards). A `None`
  /// receiver is a value of unknown provenance.
  name_reads: Vec<NameRead>,
  /// The value destructured by the pattern being visited.
  pattern_source: Option<Span>,
  /// Keys with a provider whose objects were not all followed.
  incomplete_keys: FxHashSet<String>,
}

type NameRead = (Option<(u32, u32)>, String, Vec<Guard>);

impl<'a> UseCollector<'_, '_, 'a> {
  fn bail_global(&mut self, span: Span, reason: &str) {
    self.global_bailouts.push(format!("{reason} (module {:?} at {})", self.module, span.start));
  }

  /// A property read or written by `name`, `None` when the name is not constant.
  fn check_member_name(&mut self, name: Option<&str>, span: Span, on_global_object: bool) {
    match name {
      Some(name) if CONTEXT_INTERNALS.contains(&name) => {
        self.bail_global(span, &format!("`{name}` may read Context internals"));
      }
      Some(name) if REFLECTIVE_MEMBERS.contains(&name) => {
        self.bail_global(span, &format!("`{name}` may reach a Context getter"));
      }
      Some(name) if CODE_LOADERS.contains(&name) => {
        self.bail_global(span, &format!("`{name}` may run code outside the bundle"));
      }
      // The global object holds `eval` and `Function`. Other non-constant
      // names are checked against the object's provenance.
      None if on_global_object => {
        self.bail_global(span, "global object read by a non-constant name");
      }
      Some(_) | None => {}
    }
  }

  /// A destructured property key.
  fn check_property_key(&mut self, key: &PropertyKey<'a>, computed: bool, span: Span) {
    let name = if computed {
      key.as_expression().and_then(|expr| self.shared.member_key(self.module, expr))
    } else {
      key.static_name().and_then(|n| n.as_js_str().as_str().map(String::from))
    };
    self.check_member_name(name.as_deref(), span, false);
  }

  fn property_name(&self, key: &PropertyKey<'a>, computed: bool) -> Option<String> {
    if computed {
      key.as_expression().and_then(|expr| self.shared.const_string(self.module, expr))
    } else {
      key.static_name().and_then(|n| n.as_js_str().as_str().map(String::from))
    }
  }

  /// A read of `name` on the value at `receiver`, or on an unknown value. It
  /// demands `name` on every service if the value may hold one.
  fn read_name(&mut self, receiver: Option<Span>, name: &str) {
    self.name_reads.push((
      receiver.map(|span| (span.start, span.end)),
      name.to_string(),
      self.guards.clone(),
    ));
  }

  fn is_global(&self, expr: &Expression<'_>, names: &[&str]) -> bool {
    matches!(strip(expr), Expression::Identifier(ident)
      if names.contains(&ident.name.as_str()) && self.shared.resolve_ident(self.module, ident).is_none())
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
  fn effect_sites(&self, expr: &Expression<'a>, found: &mut Vec<FoundSite>) -> bool {
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
        for (span, mut bindings, existing_top) in acquired {
          bindings.extend(binding);
          found.push((span, bindings, existing_top || top));
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
      complete &= self.value_sites(value, found);
    }
    complete
  }

  fn value_sites(&self, value: &Expression<'a>, found: &mut Vec<FoundSite>) -> bool {
    match strip(value) {
      Expression::ObjectExpression(object) => {
        found.push((object.span.start, vec![], false));
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
  /// Sources that are not recognised object literals are simply not sites, so
  /// whether every source was followed does not matter here.
  fn provider(&mut self, key: &str, kind: &str, value: &Expression<'a>, container: Container) {
    let mut found = vec![];
    let complete = if kind == "Layer.succeed" {
      self.value_sites(value, &mut found)
    } else {
      self.effect_sites(value, &mut found)
    };
    if !complete || found.is_empty() {
      self.incomplete_keys.insert(key.to_string());
    }
    for (start, bindings, top) in found {
      let index = *self.site_spans.entry(start).or_insert_with(|| {
        self.sites.push(Site {
          module: self.module,
          start,
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
      site.bindings.extend(bindings);
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
    // `ns.binding` is a use of the exported binding, from the member's position.
    if let Some(symbol) = symbol {
      let canonical = self.shared.symbols.canonical_ref_for((self.module, symbol).into());
      if let Resolved::Namespace(target) = self.shared.as_resolved(canonical) {
        if let NamespaceChain::Export { symbol: export, span, depth } =
          self.namespace_chain(ident.span, target)
        {
          let container = match self.path.len().checked_sub(2 + depth) {
            Some(index) => self.container_from(span, index),
            None => Container::Escape,
          };
          self.namespace_refs.push((
            export,
            RefInfo {
              span,
              top_stmt: self.top_stmt,
              chain: self.guards.clone(),
              member: None,
              container,
              root_symbol: None,
            },
          ));
        }
      }
    }
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
    // `Reflect.get(context, "mapUnsafe")`, `Object.getOwnPropertyDescriptor(proto, "mapUnsafe")`.
    if !self.trusted {
      if let Expression::StaticMemberExpression(member) = callee {
        let builtin = self.shared.builtin_path(self.module, &member.object);
        if matches!(builtin.as_deref(), Some("Object" | "Reflect")) {
          for argument in &it.arguments {
            let name =
              argument.as_expression().and_then(|expr| self.shared.const_string(self.module, expr));
            if let Some(name) = name.filter(|name| {
              CONTEXT_INTERNALS.contains(&name.as_str()) || CODE_LOADERS.contains(&name.as_str())
            }) {
              self.check_member_name(Some(&name), it.span, false);
            }
          }
        }
      }
    }
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
            Expression::FunctionExpression(function) => (true, reads_this(function)),
            Expression::Identifier(_) => {
              // A binding known to hold a function that never reads `this`.
              let resolved = self.shared.resolve_expr(self.module, value);
              let this_free = matches!(resolved, Some(Resolved::Symbol(symbol)) if self.shared.this_free.contains(&symbol));
              (true, !this_free)
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
          if this_risk || sets_prototype || property.kind != PropertyKind::Init {
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
    let kind = AstKind::VariableDeclarator(self.alloc(it));
    self.enter_node(kind);
    // Destructured names are read from the initializer.
    let previous = self.pattern_source.take();
    self.pattern_source = it.init.as_ref().map(GetSpan::span);
    self.visit_binding_pattern(&it.id);
    self.pattern_source = previous;
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
      let member_written = match self.parent(use_depth) {
        Some(AstKind::StaticMemberExpression(member)) if member.object.span() == use_span => {
          self.is_write_target(use_depth + 1)
        }
        _ => false,
      };
      if member_written {
        self.bail_global(use_span, "service key member written");
      }
      self.classify_key_reference(&key, use_span, use_depth);
    }
  }

  fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
    if !self.trusted
      && !matches!(self.shared.resolve_expr(self.module, &it.object), Some(Resolved::Namespace(_)))
    {
      self.check_member_name(Some(it.property.name.as_str()), it.span, false);
      self.read_name(Some(it.object.span()), it.property.name.as_str());
    }
    walk_js::walk_static_member_expression(self, it);
  }

  fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
    if !self.trusted {
      let name = self.shared.member_key(self.module, &it.expression);
      let on_global_object = self.is_global(&it.object, GLOBAL_OBJECTS);
      self.check_member_name(name.as_deref(), it.span, on_global_object);
      if let Some(name) = self.shared.const_string(self.module, &it.expression) {
        self.read_name(Some(it.object.span()), &name);
      }
    }
    walk_js::walk_computed_member_expression(self, it);
  }

  fn visit_binding_property(&mut self, it: &BindingProperty<'a>) {
    if !self.trusted {
      self.check_property_key(&it.key, it.computed, it.span);
      if let Some(name) = self.property_name(&it.key, it.computed) {
        self.read_name(self.pattern_source, &name);
      }
    }
    walk_js::walk_binding_property(self, it);
  }

  fn visit_assignment_target_property_property(
    &mut self,
    it: &AssignmentTargetPropertyProperty<'a>,
  ) {
    if !self.trusted {
      self.check_property_key(&it.name, it.computed, it.span);
      if let Some(name) = self.property_name(&it.name, it.computed) {
        self.read_name(None, &name);
      }
    }
    walk_js::walk_assignment_target_property_property(self, it);
  }

  fn visit_assignment_target_property_identifier(
    &mut self,
    it: &AssignmentTargetPropertyIdentifier<'a>,
  ) {
    if !self.trusted {
      self.check_member_name(Some(it.binding.name.as_str()), it.span, false);
      self.read_name(None, it.binding.name.as_str());
    }
    walk_js::walk_assignment_target_property_identifier(self, it);
  }

  fn visit_formal_parameters(&mut self, it: &FormalParameters<'a>) {
    let previous = self.pattern_source.take();
    walk_js::walk_formal_parameters(self, it);
    self.pattern_source = previous;
  }

  fn visit_with_statement(&mut self, it: &WithStatement<'a>) {
    if !self.trusted {
      self.bail_global(it.span, "`with` reads properties by name");
    }
    walk_js::walk_with_statement(self, it);
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

// == provenance
//
// A value in user code is clean when it provably holds no Effect runtime
// object: no Context, Fiber or service, and nothing that leads to one. Only
// literals, user data built from them, Node and ECMAScript builtins applied to
// clean values, and the success values of a few modelled Effect constructors
// are clean. Every other value, including the result of any other trusted call
// and every parameter the analysis cannot bind, may hold a runtime object.
// Reflection, serialization and formatting only disable the pass when their
// operand may hold one, and static reads on such a value demand their name on
// every service.

/// Where a local binding's value comes from.
enum Source<'a> {
  /// The value of an expression, or a part of it (destructuring, `for...of`).
  Expr(&'a Expression<'a>),
  /// A primitive: `for...in` keys, `++`, compound assignment, `undefined`.
  Primitive {
    numeric: bool,
  },
  /// A function declaration.
  Function,
  Unknown,
}

#[derive(Clone, Copy)]
enum FnNode<'a> {
  Arrow(&'a ArrowFunctionExpression<'a>),
  Function(&'a Function<'a>),
}

impl<'a> FnNode<'a> {
  fn is_generator(self) -> bool {
    matches!(self, FnNode::Function(f) if f.generator)
  }

  /// Returned expressions, or `None` when not every return can be followed.
  fn returns(self) -> Option<Vec<&'a Expression<'a>>> {
    let body = match self {
      FnNode::Arrow(f) => {
        if let Some(expr) = f.get_expression() {
          return Some(vec![expr]);
        }
        f.body.as_function_body()?
      }
      FnNode::Function(f) => f.body.as_ref()?,
    };
    let mut returns = Returns(vec![], true);
    returns.statements(&body.statements);
    returns.1.then_some(returns.0)
  }

  fn reads_arguments(self) -> bool {
    match self {
      FnNode::Arrow(_) => false,
      FnNode::Function(f) => f.body.as_ref().is_none_or(|body| reads_arguments(body)),
    }
  }
}

#[derive(Default)]
struct FnUses<'a> {
  calls: Vec<&'a ArenaVec<'a, Argument<'a>>>,
  escaped: bool,
}

/// Value flow of one untrusted module.
#[derive(Default)]
struct Flow<'a> {
  sources: FxHashMap<SymbolId, Vec<Source<'a>>>,
  /// Simple parameters: symbol -> (function start, index).
  params: FxHashMap<SymbolId, (u32, usize)>,
  /// First parameter of an inline callback of `Effect.map` / `flatMap` / `tap`
  /// -> the effect whose success it receives.
  callback_params: FxHashMap<SymbolId, &'a Expression<'a>>,
  functions: FxHashMap<u32, FnNode<'a>>,
  /// `const f = () => ...` and function declarations -> function start.
  named: FxHashMap<SymbolId, u32>,
  named_by_function: FxHashMap<u32, SymbolId>,
  uses: FxHashMap<SymbolId, FnUses<'a>>,
  /// `yield*` expressions directly inside an Effect generator.
  effect_yields: FxHashSet<u32>,
  /// Values stored into objects or collections.
  heap_writes: Vec<&'a Expression<'a>>,
  heap_unknown: bool,
  /// Imports from Node modules that format their arguments.
  formatting_imports: FxHashSet<SymbolId>,
}

// Methods that store their arguments in the receiver.
const MUTATORS: &[&str] = &[
  "push",
  "unshift",
  "splice",
  "fill",
  "copyWithin",
  "set",
  "add",
  "assign",
  "defineProperty",
  "defineProperties",
  "setPrototypeOf",
  "__defineGetter__",
  "__defineSetter__",
  "call",
  "apply",
];
// Methods whose result is clean when the receiver and arguments are.
const CLEAN_METHODS: &[&str] = &[
  "join",
  "slice",
  "concat",
  "filter",
  "find",
  "findLast",
  "at",
  "flat",
  "includes",
  "indexOf",
  "lastIndexOf",
  "some",
  "every",
  "toString",
  "toFixed",
  "toLowerCase",
  "toUpperCase",
  "trim",
  "trimStart",
  "trimEnd",
  "split",
  "replace",
  "replaceAll",
  "padStart",
  "padEnd",
  "repeat",
  "startsWith",
  "endsWith",
  "charAt",
  "charCodeAt",
  "substring",
  "get",
  "has",
  "keys",
  "values",
  "entries",
];
// Builtins that read, serialize or format their arguments, by path. Calling
// one with a value that may hold a runtime object disables the pass, and so
// does any other use of them, which hides the call from the analysis.
const READING_BUILTINS: &[&str] = &[
  "Object.keys",
  "Object.values",
  "Object.entries",
  "Object.getOwnPropertyNames",
  "Object.getOwnPropertyDescriptor",
  "Object.getOwnPropertyDescriptors",
  "Object.getOwnPropertySymbols",
  "Object.hasOwn",
  "Reflect",
  "JSON.stringify",
  "structuredClone",
  "String",
  "console",
  "Error",
  "TypeError",
  "RangeError",
  "SyntaxError",
  "ReferenceError",
  "EvalError",
  "URIError",
  "AggregateError",
];
// Trusted APIs that format their arguments: logs and the pretty-printed Cause.
const FORMATTING_API_PREFIXES: &[&str] =
  &["Effect.fail", "Effect.die", "Effect.log", "Console.", "Inspectable.", "Cause.", "Logger."];
// Node modules that format their arguments.
const FORMATTING_MODULES: &[&str] = &["util", "console"];
// Methods that format their receiver.
const FORMATTING_METHODS: &[&str] = &["toString", "toLocaleString", "join"];
// String methods that format their arguments.
const COERCING_METHODS: &[&str] = &[
  "concat",
  "padStart",
  "padEnd",
  "replace",
  "replaceAll",
  "localeCompare",
  "match",
  "matchAll",
  "search",
  "split",
  "startsWith",
  "endsWith",
  "includes",
  "indexOf",
  "lastIndexOf",
];
// Methods that observe whether a property exists.
const PRESENCE_METHODS: &[&str] = &["hasOwnProperty", "propertyIsEnumerable"];

fn path_matches(path: &str, prefix: &str) -> bool {
  path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.'))
}

impl Shared<'_> {
  /// The path of a global builtin, `Object.values` for `Object.values` or
  /// `globalThis.Object["values"]`; `globalThis` for the global object itself.
  fn builtin_path(&self, module: ModuleIdx, expr: &Expression<'_>) -> Option<String> {
    let join = |base: String, name: &str| {
      if GLOBAL_OBJECTS.contains(&base.as_str()) {
        name.to_string()
      } else {
        format!("{base}.{name}")
      }
    };
    match strip(expr) {
      Expression::Identifier(ident) => (ident.reference_id.get().is_some()
        && self.resolve_ident(module, ident).is_none())
      .then(|| ident.name.to_string()),
      Expression::StaticMemberExpression(member) => {
        Some(join(self.builtin_path(module, &member.object)?, member.property.name.as_str()))
      }
      Expression::ComputedMemberExpression(member) => {
        let base = self.builtin_path(module, &member.object)?;
        Some(join(base, &self.const_string(module, &member.expression)?))
      }
      _ => None,
    }
  }
}

fn collect_flow<'a>(shared: &Shared<'_>, module: ModuleIdx, program: &Program<'a>) -> Flow<'a> {
  let mut collector = FlowCollector {
    shared,
    module,
    flow: Flow::default(),
    kind: VariableDeclarationKind::Var,
    callee_spans: FxHashSet::default(),
    effect_generators: FxHashSet::default(),
    functions: vec![],
    loop_head: None,
  };
  collector.visit_program(program);
  collector.flow
}

/// Pass C: value flow of an untrusted module.
struct FlowCollector<'s, 'l, 'a> {
  shared: &'s Shared<'l>,
  module: ModuleIdx,
  flow: Flow<'a>,
  kind: VariableDeclarationKind,
  callee_spans: FxHashSet<u32>,
  effect_generators: FxHashSet<u32>,
  /// Whether each enclosing function is an Effect generator.
  functions: Vec<bool>,
  /// Bindings introduced by a `for...in` / `for...of` head: the iterated value.
  loop_head: Option<(bool, &'a Expression<'a>)>,
}

impl<'a> FlowCollector<'_, '_, 'a> {
  fn symbol(&self, ident: &IdentifierReference<'_>) -> Option<SymbolId> {
    let reference = ident.reference_id.get()?;
    self.shared.scoping(self.module).get_reference(reference).symbol_id()
  }

  fn add(&mut self, symbol: SymbolId, source: Source<'a>) {
    self.flow.sources.entry(symbol).or_default().push(source);
  }

  /// Bindings of a pattern, all from `value`.
  fn bind_pattern(&mut self, pattern: &BindingPattern<'a>, value: Option<&'a Expression<'a>>) {
    let has_default =
      matches!(pattern, BindingPattern::AssignmentPattern(_)) || pattern_has_default(pattern);
    for ident in pattern.get_binding_identifiers() {
      let source = match value {
        Some(value) if !has_default => Source::Expr(value),
        None if !has_default && matches!(pattern, BindingPattern::BindingIdentifier(_)) => {
          Source::Primitive { numeric: true }
        }
        _ => Source::Unknown,
      };
      self.add(ident.symbol_id(), source);
    }
  }

  fn function(&mut self, node: FnNode<'a>, start: u32, params: &'a FormalParameters<'a>) {
    self.flow.functions.insert(start, node);
    for (index, param) in params.items.iter().enumerate() {
      match &param.pattern {
        BindingPattern::BindingIdentifier(id) if param.initializer.is_none() => {
          self.flow.params.insert(id.symbol_id(), (start, index));
        }
        pattern => {
          for ident in pattern.get_binding_identifiers() {
            self.add(ident.symbol_id(), Source::Unknown);
          }
        }
      }
    }
    if let Some(rest) = &params.rest {
      for ident in rest.rest.argument.get_binding_identifiers() {
        self.add(ident.symbol_id(), Source::Unknown);
      }
    }
  }

  fn assignment_target(
    &mut self,
    target: &'a AssignmentTarget<'a>,
    value: Option<&'a Expression<'a>>,
  ) {
    match target {
      AssignmentTarget::AssignmentTargetIdentifier(ident) => {
        if let Some(symbol) = self.symbol(ident) {
          self.add(symbol, value.map_or(Source::Unknown, Source::Expr));
        }
      }
      AssignmentTarget::StaticMemberExpression(_)
      | AssignmentTarget::ComputedMemberExpression(_)
      | AssignmentTarget::PrivateFieldExpression(_) => match value {
        Some(value) => self.flow.heap_writes.push(value),
        None => self.flow.heap_unknown = true,
      },
      AssignmentTarget::ArrayAssignmentTarget(_) | AssignmentTarget::ObjectAssignmentTarget(_) => {
        // Every identifier inside takes a part of the value; members are heap writes.
        let mut finder = TargetFinder { identifiers: vec![], members: false, defaults: false };
        finder.visit_assignment_target(target);
        if finder.members {
          self.flow.heap_unknown = true;
        }
        for ident in finder.identifiers {
          if let Some(symbol) = self.symbol(ident) {
            let source = match value {
              Some(value) if !finder.defaults => Source::Expr(value),
              _ => Source::Unknown,
            };
            self.add(symbol, source);
          }
        }
      }
      _ => self.flow.heap_unknown = true,
    }
  }
}

fn pattern_has_default(pattern: &BindingPattern<'_>) -> bool {
  match pattern {
    BindingPattern::BindingIdentifier(_) => false,
    BindingPattern::AssignmentPattern(_) => true,
    BindingPattern::ObjectPattern(object) => {
      object.properties.iter().any(|property| pattern_has_default(&property.value))
        || object.rest.as_ref().is_some_and(|rest| pattern_has_default(&rest.argument))
    }
    BindingPattern::ArrayPattern(array) => {
      array.elements.iter().flatten().any(pattern_has_default)
        || array.rest.as_ref().is_some_and(|rest| pattern_has_default(&rest.argument))
    }
  }
}

/// Identifiers and members written by a destructuring assignment target.
struct TargetFinder<'a> {
  identifiers: Vec<&'a IdentifierReference<'a>>,
  members: bool,
  defaults: bool,
}

impl<'a> VisitJs<'a> for TargetFinder<'a> {
  fn visit_simple_assignment_target(&mut self, it: &SimpleAssignmentTarget<'a>) {
    match it {
      SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => {
        self.identifiers.push(self.alloc(ident.as_ref()));
      }
      _ => self.members = true,
    }
  }
  fn visit_assignment_target_with_default(&mut self, it: &AssignmentTargetWithDefault<'a>) {
    self.defaults = true;
    walk_js::walk_assignment_target_with_default(self, it);
  }
  fn visit_assignment_target_property_identifier(
    &mut self,
    it: &AssignmentTargetPropertyIdentifier<'a>,
  ) {
    self.defaults |= it.init.is_some();
    self.identifiers.push(self.alloc(&it.binding));
  }
  fn visit_expression(&mut self, _it: &Expression<'a>) {}
}

impl<'a> VisitJs<'a> for FlowCollector<'_, '_, 'a> {
  fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'a>) {
    let previous = self.kind;
    self.kind = it.kind;
    walk_js::walk_variable_declaration(self, it);
    self.kind = previous;
  }

  fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
    let it = self.alloc(it);
    match self.loop_head.take() {
      Some((true, iterated)) => self.bind_pattern(&it.id, Some(iterated)),
      Some((false, _)) => {
        for ident in it.id.get_binding_identifiers() {
          self.add(ident.symbol_id(), Source::Primitive { numeric: false });
        }
      }
      None => {
        self.bind_pattern(&it.id, it.init.as_ref());
        if let (BindingPattern::BindingIdentifier(id), Some(init), VariableDeclarationKind::Const) =
          (&it.id, &it.init, self.kind)
        {
          if let Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) =
            strip(init)
          {
            let start = strip(init).span().start;
            self.flow.named.insert(id.symbol_id(), start);
            self.flow.named_by_function.insert(start, id.symbol_id());
          }
        }
      }
    }
    walk_js::walk_variable_declarator(self, it);
  }

  fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
    let it = self.alloc(it);
    match &it.left {
      ForStatementLeft::VariableDeclaration(_) => self.loop_head = Some((false, &it.right)),
      left => {
        if let Some(AssignmentTarget::AssignmentTargetIdentifier(ident)) =
          left.as_assignment_target()
        {
          if let Some(symbol) = self.symbol(ident) {
            self.add(symbol, Source::Primitive { numeric: false });
          }
        }
      }
    }
    walk_js::walk_for_in_statement(self, it);
    self.loop_head = None;
  }

  fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
    let it = self.alloc(it);
    match &it.left {
      ForStatementLeft::VariableDeclaration(_) => self.loop_head = Some((true, &it.right)),
      left => {
        if let Some(target) = left.as_assignment_target() {
          self.assignment_target(target, Some(&it.right));
        }
      }
    }
    walk_js::walk_for_of_statement(self, it);
    self.loop_head = None;
  }

  fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
    let it = self.alloc(it);
    if it.operator == AssignmentOperator::Assign || it.operator.is_logical() {
      self.assignment_target(&it.left, Some(&it.right));
    } else if let AssignmentTarget::AssignmentTargetIdentifier(ident) = &it.left {
      // `x += y` is a string or a number.
      let numeric = it.operator != AssignmentOperator::Addition || is_numeric(&it.right);
      if let Some(symbol) = self.symbol(ident) {
        self.add(symbol, Source::Primitive { numeric });
      }
    }
    walk_js::walk_assignment_expression(self, it);
  }

  fn visit_update_expression(&mut self, it: &UpdateExpression<'a>) {
    if let SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) = &it.argument {
      if let Some(symbol) = self.symbol(ident) {
        self.add(symbol, Source::Primitive { numeric: true });
      }
    }
    walk_js::walk_update_expression(self, it);
  }

  fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
    if let Some(param) = &it.param {
      for ident in param.pattern.get_binding_identifiers() {
        self.add(ident.symbol_id(), Source::Unknown);
      }
    }
    walk_js::walk_catch_clause(self, it);
  }

  fn visit_class(&mut self, it: &Class<'a>) {
    if let Some(id) = &it.id {
      self.add(id.symbol_id(), Source::Unknown);
    }
    walk_js::walk_class(self, it);
  }

  fn visit_function(&mut self, it: &Function<'a>, flags: oxc::semantic::ScopeFlags) {
    let it = self.alloc(it);
    self.function(FnNode::Function(it), it.span.start, &it.params);
    if let (Some(id), FunctionType::FunctionDeclaration) = (&it.id, it.r#type) {
      self.add(id.symbol_id(), Source::Function);
      if !self.shared.scoping(self.module).symbol_is_mutated(id.symbol_id()) {
        self.flow.named.insert(id.symbol_id(), it.span.start);
        self.flow.named_by_function.insert(it.span.start, id.symbol_id());
      }
    }
    self.functions.push(it.generator && self.effect_generators.contains(&it.span.start));
    walk_js::walk_function(self, it, flags);
    self.functions.pop();
  }

  fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
    let it = self.alloc(it);
    self.function(FnNode::Arrow(it), it.span.start, &it.params);
    self.functions.push(false);
    walk_js::walk_arrow_function_expression(self, it);
    self.functions.pop();
  }

  fn visit_yield_expression(&mut self, it: &YieldExpression<'a>) {
    if it.delegate && self.functions.last() == Some(&true) {
      self.flow.effect_yields.insert(it.span.start);
    }
    walk_js::walk_yield_expression(self, it);
  }

  fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
    let it = self.alloc(it);
    let callee = strip(&it.callee);
    if let Expression::Identifier(ident) = callee {
      self.callee_spans.insert(ident.span.start);
      if let Some(symbol) = self.symbol(ident) {
        self.flow.uses.entry(symbol).or_default().calls.push(&it.arguments);
      }
    }
    if let Expression::StaticMemberExpression(member) = callee {
      if MUTATORS.contains(&member.property.name.as_str()) {
        for argument in &it.arguments {
          match argument {
            Argument::SpreadElement(spread) => self.flow.heap_writes.push(&spread.argument),
            argument => {
              if let Some(expr) = argument.as_expression() {
                self.flow.heap_writes.push(expr);
              }
            }
          }
        }
      }
    }
    match self.shared.api_of(self.module, callee) {
      Some("Effect.gen" | "Effect.fn" | "Effect.fnUntraced") => {
        for argument in &it.arguments {
          if let Some(Expression::FunctionExpression(f)) = argument.as_expression().map(strip) {
            self.effect_generators.insert(f.span.start);
          }
        }
      }
      Some("Effect.map" | "Effect.flatMap" | "Effect.tap") if it.arguments.len() == 2 => {
        let first_param = match it.arguments[1].as_expression().map(strip) {
          Some(Expression::ArrowFunctionExpression(f)) => f.params.items.first(),
          Some(Expression::FunctionExpression(f)) => f.params.items.first(),
          _ => None,
        };
        if let (Some(param), Some(effect)) = (first_param, it.arguments[0].as_expression()) {
          if let (BindingPattern::BindingIdentifier(id), None) =
            (&param.pattern, &param.initializer)
          {
            self.flow.callback_params.insert(id.symbol_id(), effect);
          }
        }
      }
      _ => {}
    }
    walk_js::walk_call_expression(self, it);
  }

  fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
    let source = it.source.value.as_str().unwrap_or_default();
    let source = source.strip_prefix("node:").unwrap_or(source);
    if FORMATTING_MODULES.contains(&source) {
      for specifier in it.specifiers.iter().flatten() {
        self.flow.formatting_imports.insert(specifier.local().symbol_id());
      }
    }
  }

  fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
    if !self.callee_spans.contains(&it.span.start) {
      if let Some(symbol) = self.symbol(it) {
        self.flow.uses.entry(symbol).or_default().escaped = true;
      }
    }
  }
}

/// Whether values in an untrusted module may hold an Effect runtime object.
struct Prov<'f, 's, 'l, 'a> {
  shared: &'s Shared<'l>,
  module: ModuleIdx,
  flow: &'f Flow<'a>,
  heap_tainted: bool,
  /// Key -> method -> whether every provider's method returns a clean value.
  facts: &'f FxHashMap<String, FxHashMap<String, bool>>,
  /// Consumer bindings of this module -> key.
  consumers: &'f FxHashMap<SymbolId, String>,
  symbols: FxHashMap<SymbolId, Option<bool>>,
  numeric_symbols: FxHashMap<SymbolId, Option<bool>>,
  returns: FxHashMap<u32, Option<bool>>,
  depth: usize,
}

impl<'f, 's, 'l, 'a> Prov<'f, 's, 'l, 'a> {
  fn new(
    shared: &'s Shared<'l>,
    module: ModuleIdx,
    flow: &'f Flow<'a>,
    heap_tainted: bool,
    facts: &'f FxHashMap<String, FxHashMap<String, bool>>,
    consumers: &'f FxHashMap<SymbolId, String>,
  ) -> Self {
    Prov {
      shared,
      module,
      flow,
      heap_tainted,
      facts,
      consumers,
      symbols: FxHashMap::default(),
      numeric_symbols: FxHashMap::default(),
      returns: FxHashMap::default(),
      depth: 0,
    }
  }

  fn symbol(&self, ident: &IdentifierReference<'_>) -> Option<SymbolId> {
    let reference = ident.reference_id.get()?;
    self.shared.scoping(self.module).get_reference(reference).symbol_id()
  }

  fn args_clean(&mut self, args: &[Argument<'a>]) -> bool {
    args.iter().all(|argument| match argument {
      Argument::SpreadElement(spread) => self.clean(&spread.argument),
      argument => argument.as_expression().is_some_and(|expr| self.clean(expr)),
    })
  }

  fn clean(&mut self, expr: &Expression<'a>) -> bool {
    if self.depth > 64 {
      return false;
    }
    self.depth += 1;
    let result = self.clean_inner(expr);
    self.depth -= 1;
    result
  }

  fn clean_inner(&mut self, expr: &Expression<'a>) -> bool {
    match strip(expr) {
      Expression::StringLiteral(_)
      | Expression::NumericLiteral(_)
      | Expression::BooleanLiteral(_)
      | Expression::NullLiteral(_)
      | Expression::BigIntLiteral(_)
      | Expression::RegExpLiteral(_)
      | Expression::TemplateLiteral(_)
      | Expression::UnaryExpression(_)
      | Expression::BinaryExpression(_)
      | Expression::UpdateExpression(_)
      | Expression::ArrowFunctionExpression(_)
      | Expression::FunctionExpression(_) => true,
      Expression::Identifier(ident) => match self.symbol(ident) {
        Some(symbol) => self.symbol_clean(symbol),
        None => {
          ident.reference_id.get().is_some()
            && matches!(ident.name.as_str(), "undefined" | "NaN" | "Infinity")
        }
      },
      Expression::ArrayExpression(array) => {
        !self.heap_tainted
          && array.elements.iter().all(|element| match element {
            ArrayExpressionElement::Elision(_) => true,
            ArrayExpressionElement::SpreadElement(spread) => self.clean(&spread.argument),
            element => element.as_expression().is_some_and(|expr| self.clean(expr)),
          })
      }
      Expression::ObjectExpression(object) => {
        !self.heap_tainted
          && object.properties.iter().all(|property| match property {
            ObjectPropertyKind::SpreadProperty(spread) => self.clean(&spread.argument),
            ObjectPropertyKind::ObjectProperty(property) => {
              property.kind == PropertyKind::Init
                && property
                  .key
                  .static_name()
                  .and_then(|name| name.as_js_str().as_str().map(String::from))
                  .as_deref()
                  != Some("__proto__")
                && (!property.computed
                  || property.key.as_expression().is_some_and(|key| self.clean(key)))
                && self.clean(&property.value)
            }
          })
      }
      Expression::LogicalExpression(logical) => {
        self.clean(&logical.left) && self.clean(&logical.right)
      }
      Expression::ConditionalExpression(conditional) => {
        self.clean(&conditional.consequent) && self.clean(&conditional.alternate)
      }
      Expression::SequenceExpression(sequence) => {
        sequence.expressions.last().is_some_and(|last| self.clean(last))
      }
      Expression::AssignmentExpression(assign) => {
        !(assign.operator == AssignmentOperator::Assign || assign.operator.is_logical())
          || self.clean(&assign.right)
      }
      Expression::AwaitExpression(await_expr) => self.clean(&await_expr.argument),
      Expression::YieldExpression(yield_expr) => {
        yield_expr.delegate
          && self.flow.effect_yields.contains(&yield_expr.span.start)
          && yield_expr.argument.as_ref().is_some_and(|argument| self.success(argument))
      }
      Expression::StaticMemberExpression(member) => {
        self.shared.resolve_expr(self.module, expr).is_none() && self.clean(&member.object)
      }
      Expression::ComputedMemberExpression(member) => self.clean(&member.object),
      Expression::ChainExpression(chain) => match &chain.expression {
        ChainElement::CallExpression(call) => self.call_clean(call),
        ChainElement::StaticMemberExpression(member) => self.clean(&member.object),
        ChainElement::ComputedMemberExpression(member) => self.clean(&member.object),
        _ => false,
      },
      Expression::CallExpression(call) => self.call_clean(call),
      Expression::NewExpression(new) => {
        matches!(
          self.shared.builtin_path(self.module, &new.callee).as_deref(),
          Some("Map" | "Set" | "Date" | "Array" | "URL" | "URLSearchParams" | "RegExp")
        ) && self.args_clean(&new.arguments)
      }
      _ => false,
    }
  }

  fn symbol_clean(&mut self, symbol: SymbolId) -> bool {
    match self.symbols.get(&symbol) {
      Some(Some(clean)) => return *clean,
      Some(None) => return false,
      None => {}
    }
    self.symbols.insert(symbol, None);
    let flow = self.flow;
    let mut known = false;
    let mut clean = true;
    if let Some(effect) = flow.callback_params.get(&symbol) {
      known = true;
      clean &= self.success(effect);
    } else if let Some(&(function, index)) = flow.params.get(&symbol) {
      known = true;
      clean &= self.param_clean(function, index);
    }
    if let Some(sources) = flow.sources.get(&symbol) {
      known = true;
      for source in sources {
        if !clean {
          break;
        }
        clean &= match source {
          Source::Expr(expr) => self.clean(expr),
          Source::Primitive { .. } | Source::Function => true,
          Source::Unknown => false,
        };
      }
    }
    let clean = known && clean;
    self.symbols.insert(symbol, Some(clean));
    clean
  }

  /// A parameter of a function only called directly, by every call site.
  fn param_clean(&mut self, function: u32, index: usize) -> bool {
    let flow = self.flow;
    let Some(symbol) = flow.named_by_function.get(&function) else { return false };
    let Some(node) = flow.functions.get(&function) else { return false };
    if node.reads_arguments() {
      return false;
    }
    // Other modules may call an exported function with anything.
    let this: SymbolRef = (self.module, *symbol).into();
    let exported = self.shared.resolved_exports[self.module.index()]
      .values()
      .any(|export| self.shared.symbols.canonical_ref_for(*export) == this);
    if exported {
      return false;
    }
    let Some(uses) = flow.uses.get(symbol) else { return true };
    if uses.escaped {
      return false;
    }
    uses.calls.iter().all(|args| {
      if args.iter().take(index + 1).any(|arg| matches!(arg, Argument::SpreadElement(_))) {
        return false;
      }
      match args.get(index) {
        None => true,
        Some(arg) => arg.as_expression().is_some_and(|expr| self.clean(expr)),
      }
    })
  }

  fn returns_clean(&mut self, node: FnNode<'a>, success: bool) -> bool {
    let Some(returns) = node.returns() else { return false };
    returns.into_iter().all(|expr| if success { self.success(expr) } else { self.clean(expr) })
  }

  /// The result of calling a named function.
  fn function_clean(&mut self, function: u32) -> bool {
    match self.returns.get(&function) {
      Some(Some(clean)) => return *clean,
      Some(None) => return false,
      None => {}
    }
    let Some(&node) = self.flow.functions.get(&function) else { return false };
    self.returns.insert(function, None);
    let clean = !node.is_generator() && self.returns_clean(node, false);
    self.returns.insert(function, Some(clean));
    clean
  }

  fn function_expr(&self, expr: &Expression<'a>) -> Option<FnNode<'a>> {
    let start = match strip(expr) {
      Expression::ArrowFunctionExpression(f) => f.span.start,
      Expression::FunctionExpression(f) => f.span.start,
      Expression::Identifier(ident) => *self.flow.named.get(&self.symbol(ident)?)?,
      _ => return None,
    };
    self.flow.functions.get(&start).copied()
  }

  fn consumer_key(&self, expr: &Expression<'a>) -> Option<&String> {
    match strip(expr) {
      Expression::Identifier(ident) => self.consumers.get(&self.symbol(ident)?),
      Expression::YieldExpression(yield_expr) if yield_expr.delegate => {
        self.shared.key_of(self.module, yield_expr.argument.as_ref()?)
      }
      _ => None,
    }
  }

  fn call_clean(&mut self, call: &CallExpression<'a>) -> bool {
    let callee = strip(&call.callee);
    if let Expression::Identifier(ident) = callee {
      if let Some(&function) = self.symbol(ident).and_then(|symbol| self.flow.named.get(&symbol)) {
        return self.function_clean(function);
      }
    }
    if let Some(path) = self.shared.builtin_path(self.module, callee) {
      return match path.as_str() {
        "JSON.parse" => call.arguments.len() == 1,
        "String" | "Number" | "Boolean" | "parseInt" | "parseFloat" | "isNaN" | "isFinite"
        | "encodeURIComponent" | "decodeURIComponent" | "encodeURI" | "decodeURI"
        | "Array.isArray" | "Object.is" | "Reflect.ownKeys" | "Reflect.has" | "Date.now" => true,
        "structuredClone"
        | "Object.keys"
        | "Object.values"
        | "Object.entries"
        | "Object.getOwnPropertyNames"
        | "Object.fromEntries"
        | "Object.freeze"
        | "Array.from"
        | "Array.of" => self.args_clean(&call.arguments),
        path => path.starts_with("Math.") || path.starts_with("Number."),
      };
    }
    if let Expression::StaticMemberExpression(member) = callee {
      let name = member.property.name.as_str();
      if let Some(key) = self.consumer_key(&member.object) {
        return self.facts.get(key).and_then(|methods| methods.get(name)).copied().unwrap_or(false);
      }
      if name == "of" && self.shared.key_of(self.module, &member.object).is_some() {
        return call.arguments.len() == 1 && self.args_clean(&call.arguments);
      }
      if CLEAN_METHODS.contains(&name) {
        return self.clean(&member.object) && self.args_clean(&call.arguments);
      }
    }
    match self.shared.api_of(self.module, callee) {
      Some("Effect.runPromise" | "Effect.runSync") => {
        call.arguments.first().and_then(Argument::as_expression).is_some_and(|e| self.success(e))
      }
      _ => false,
    }
  }

  /// Whether the success value of an Effect expression is clean.
  fn success(&mut self, expr: &Expression<'a>) -> bool {
    if self.depth > 64 {
      return false;
    }
    self.depth += 1;
    let result = self.success_inner(expr);
    self.depth -= 1;
    result
  }

  fn success_inner(&mut self, expr: &Expression<'a>) -> bool {
    match strip(expr) {
      Expression::Identifier(ident) => {
        let flow = self.flow;
        let Some(symbol) = self.symbol(ident) else { return false };
        if flow.params.contains_key(&symbol) {
          return false;
        }
        match flow.sources.get(&symbol).map(Vec::as_slice) {
          Some([Source::Expr(init)]) => self.success(init),
          _ => false,
        }
      }
      Expression::CallExpression(call) => {
        let args = &call.arguments;
        let arg = |index: usize| args.get(index).and_then(Argument::as_expression);
        match self.shared.api_of(self.module, &call.callee) {
          Some("Effect.succeed") if args.len() == 1 => arg(0).is_some_and(|v| self.clean(v)),
          Some("Effect.sync") if args.len() == 1 => arg(0)
            .and_then(|f| self.function_expr(f))
            .is_some_and(|f| !f.is_generator() && self.returns_clean(f, false)),
          Some("Effect.gen") if !args.is_empty() => arg(args.len() - 1)
            .and_then(|f| self.function_expr(f))
            .is_some_and(|f| self.returns_clean(f, false)),
          Some("Effect.map") if args.len() == 2 => arg(1)
            .and_then(|f| self.function_expr(f))
            .is_some_and(|f| !f.is_generator() && self.returns_clean(f, false)),
          Some("Effect.flatMap") if args.len() == 2 => arg(1)
            .and_then(|f| self.function_expr(f))
            .is_some_and(|f| !f.is_generator() && self.returns_clean(f, true)),
          Some("Effect.tap" | "Effect.provide") if args.len() == 2 => {
            arg(0).is_some_and(|e| self.success(e))
          }
          Some("Effect.all") if !args.is_empty() => match arg(0).map(strip) {
            Some(Expression::ArrayExpression(array)) => array
              .elements
              .iter()
              .all(|element| element.as_expression().is_some_and(|e| self.success(e))),
            _ => false,
          },
          _ => false,
        }
      }
      _ => false,
    }
  }

  /// Whether a computed key is a number, which never names a Context internal.
  fn key_numeric(&mut self, expr: &Expression<'a>) -> bool {
    if is_numeric(expr) {
      return true;
    }
    let Expression::Identifier(ident) = strip(expr) else { return false };
    let Some(symbol) = self.symbol(ident) else { return false };
    if self.flow.params.contains_key(&symbol) || self.flow.callback_params.contains_key(&symbol) {
      return false;
    }
    match self.numeric_symbols.get(&symbol) {
      Some(Some(numeric)) => return *numeric,
      Some(None) => return true,
      None => {}
    }
    self.numeric_symbols.insert(symbol, None);
    let flow = self.flow;
    let numeric = flow.sources.get(&symbol).is_some_and(|sources| {
      sources.iter().all(|source| match source {
        Source::Expr(expr) => self.key_numeric(expr),
        Source::Primitive { numeric } => *numeric,
        Source::Function | Source::Unknown => false,
      })
    });
    self.numeric_symbols.insert(symbol, Some(numeric));
    numeric
  }
}

/// Pass D: method results of user providers, for consumers that format them.
struct FactCollector<'p, 'f, 's, 'l, 'a> {
  prov: &'p mut Prov<'f, 's, 'l, 'a>,
  /// Site object literal start -> site index.
  sites: &'p FxHashMap<u32, usize>,
  facts: Vec<(usize, String, bool)>,
}

impl<'a> VisitJs<'a> for FactCollector<'_, '_, '_, '_, 'a> {
  fn visit_object_expression(&mut self, it: &ObjectExpression<'a>) {
    if let Some(&site) = self.sites.get(&it.span.start) {
      for property in &it.properties {
        let ObjectPropertyKind::ObjectProperty(property) = property else { continue };
        if property.computed || property.kind != PropertyKind::Init {
          continue;
        }
        let Some(name) =
          property.key.static_name().and_then(|n| n.as_js_str().as_str().map(String::from))
        else {
          continue;
        };
        let clean = self
          .prov
          .function_expr(&property.value)
          .is_some_and(|f| !f.is_generator() && self.prov.returns_clean(f, false));
        self.facts.push((site, name, clean));
      }
    }
    walk_js::walk_object_expression(self, it);
  }
}

/// Pass E: sinks whose operand may hold an Effect runtime object, and static
/// reads on such values.
struct SinkChecker<'p, 'f, 's, 'l, 'a> {
  prov: &'p mut Prov<'f, 's, 'l, 'a>,
  path: Vec<AstKind<'a>>,
  /// Receivers of static reads recorded by pass B.
  receivers: &'p FxHashSet<(u32, u32)>,
  /// Consumer and release bindings, whose reads are tracked per key.
  tracked: &'p FxHashSet<SymbolId>,
  unclean_receivers: FxHashSet<(u32, u32)>,
  /// Names whose presence is observed on a value that may hold a service.
  names: Vec<String>,
  global_bailouts: Vec<String>,
}

impl<'a> SinkChecker<'_, '_, '_, '_, 'a> {
  fn bail(&mut self, span: Span, reason: &str) {
    self
      .global_bailouts
      .push(format!("{reason} (module {:?} at {})", self.prov.module, span.start));
  }

  fn parent(&self, depth: usize) -> Option<&AstKind<'a>> {
    self.path.iter().rev().nth(depth)
  }

  fn tracked_receiver(&self, expr: &Expression<'a>) -> bool {
    match strip(expr) {
      Expression::Identifier(ident) => {
        self.prov.symbol(ident).is_some_and(|symbol| self.tracked.contains(&symbol))
      }
      Expression::YieldExpression(yield_expr) => {
        yield_expr.delegate
          && yield_expr
            .argument
            .as_ref()
            .is_some_and(|argument| self.prov.shared.key_of(self.prov.module, argument).is_some())
      }
      _ => false,
    }
  }

  /// A member path rooted at an import from a Node formatting module.
  fn formatter_callee(&self, callee: &Expression<'a>) -> bool {
    let mut expr = strip(callee);
    while let Expression::StaticMemberExpression(member) = expr {
      expr = strip(&member.object);
    }
    matches!(expr, Expression::Identifier(ident)
      if self.prov.symbol(ident).is_some_and(|s| self.prov.flow.formatting_imports.contains(&s)))
  }

  fn unclean(&mut self, expr: &Expression<'a>) -> bool {
    !self.tracked_receiver(expr) && !self.prov.clean(expr)
  }

  fn any_unclean(&mut self, args: &[Argument<'a>]) -> bool {
    args.iter().any(|argument| match argument {
      Argument::SpreadElement(spread) => self.unclean(&spread.argument),
      argument => argument.as_expression().is_some_and(|expr| self.unclean(expr)),
    })
  }

  /// Whether the expression at ancestor `depth` - 1, spanning `span`, is the callee of a call.
  fn is_called(&self, span: Span, depth: usize) -> bool {
    match self.parent(depth) {
      Some(AstKind::CallExpression(call)) => call.callee.span() == span,
      Some(AstKind::NewExpression(new)) => new.callee.span() == span,
      _ => false,
    }
  }

  fn check_call(&mut self, span: Span, callee: &Expression<'a>, args: &'a [Argument<'a>]) {
    let module = self.prov.module;
    let shared = self.prov.shared;
    if let Some(path) = shared.builtin_path(module, callee) {
      if READING_BUILTINS.iter().any(|sink| path_matches(&path, sink))
        || path.contains(".prototype.")
      {
        if self.any_unclean(args) {
          self.bail(span, &format!("`{path}` may read an Effect runtime object"));
        }
        return;
      }
    }
    if let Some(name) = shared.api_of(module, callee) {
      if FORMATTING_API_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        && self.any_unclean(args)
      {
        self.bail(span, &format!("`{name}` may format an Effect runtime object"));
        return;
      }
    }
    if self.formatter_callee(callee) && self.any_unclean(args) {
      self.bail(span, "a Node formatter may format an Effect runtime object");
      return;
    }
    let export = shared.resolve_expr(module, callee).is_some();
    if let (Expression::StaticMemberExpression(member), false) = (strip(callee), export) {
      let name = member.property.name.as_str();
      if (FORMATTING_METHODS.contains(&name) || PRESENCE_METHODS.contains(&name))
        && self.unclean(&member.object)
      {
        self.bail(span, &format!("`{name}` may read an Effect runtime object"));
        return;
      }
      if COERCING_METHODS.contains(&name) && self.any_unclean(args) {
        self.bail(span, &format!("`{name}` may format an Effect runtime object"));
        return;
      }
    }
    // `descriptor(proto, "mapUnsafe")`: a constant internal name next to a runtime value.
    let internal = args.iter().any(|argument| {
      argument
        .as_expression()
        .and_then(|expr| shared.const_string(module, expr))
        .is_some_and(|name| CONTEXT_INTERNALS.contains(&name.as_str()))
    });
    if internal && (self.any_unclean(args) || self.unclean(callee)) {
      self.bail(span, "Context internal named next to an Effect runtime object");
    }
  }
}

impl<'a> VisitJs<'a> for SinkChecker<'_, '_, '_, '_, 'a> {
  fn enter_node(&mut self, kind: AstKind<'a>) {
    self.path.push(kind);
  }

  fn leave_node(&mut self, _kind: AstKind<'a>) {
    self.path.pop();
  }

  fn visit_expression(&mut self, it: &Expression<'a>) {
    let span = it.span();
    if self.receivers.contains(&(span.start, span.end)) && self.unclean(it) {
      self.unclean_receivers.insert((span.start, span.end));
    }
    walk_js::walk_expression(self, it);
  }

  fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
    let shared = self.prov.shared;
    let module = self.prov.module;
    // A builtin that reads its argument, or the global object, must be called
    // in place: any other use hides the call.
    let mut span = it.span;
    let mut depth = 0;
    let mut path = it.name.to_string();
    let global = it.reference_id.get().is_some() && shared.resolve_ident(module, it).is_none();
    let formatter = !global
      && self.prov.symbol(it).is_some_and(|s| self.prov.flow.formatting_imports.contains(&s));
    if global || formatter {
      loop {
        match self.parent(depth) {
          Some(AstKind::StaticMemberExpression(member)) if member.object.span() == span => {
            path = if GLOBAL_OBJECTS.contains(&path.as_str()) {
              member.property.name.to_string()
            } else {
              format!("{path}.{}", member.property.name)
            };
            span = member.span;
            depth += 1;
          }
          Some(AstKind::ComputedMemberExpression(member)) if member.object.span() == span => {
            let Some(name) = shared.const_string(module, &member.expression) else { break };
            path =
              if GLOBAL_OBJECTS.contains(&path.as_str()) { name } else { format!("{path}.{name}") };
            span = member.span;
            depth += 1;
          }
          _ => break,
        }
      }
      // Error classes are fine as values: `extends Error`, `instanceof Error`.
      let hidden = formatter
        || GLOBAL_OBJECTS.contains(&path.as_str())
        || matches!(path.as_str(), "Object" | "JSON")
        || READING_BUILTINS
          .iter()
          .filter(|sink| !sink.ends_with("Error"))
          .any(|sink| path_matches(&path, sink));
      let typeof_operand = matches!(self.parent(depth), Some(AstKind::UnaryExpression(unary))
        if unary.operator == UnaryOperator::Typeof);
      if hidden && !typeof_operand && !self.is_called(span, depth) {
        self.bail(span, &format!("`{path}` used other than by a direct call"));
      }
    }
  }

  fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
    let it = self.alloc(it);
    // A subclass of `Error` formats what it passes to `super`.
    if matches!(it.callee, Expression::Super(_)) && self.any_unclean(&it.arguments) {
      self.bail(it.span, "`super` call may format an Effect runtime object");
    }
    self.check_call(it.span, &it.callee, &it.arguments);
    walk_js::walk_call_expression(self, it);
  }

  fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
    let it = self.alloc(it);
    self.check_call(it.span, &it.callee, &it.arguments);
    walk_js::walk_new_expression(self, it);
  }

  fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
    if self.unclean(&it.right) {
      self.bail(it.span, "`for...in` may enumerate an Effect runtime object");
    }
    walk_js::walk_for_in_statement(self, it);
  }

  fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
    let module = self.prov.module;
    let constant = self.prov.shared.const_string(module, &it.expression).is_some();
    if !constant && !self.prov.key_numeric(&it.expression) && self.unclean(&it.object) {
      self.bail(it.span, "member of an Effect runtime object read by a non-constant name");
    }
    walk_js::walk_computed_member_expression(self, it);
  }

  fn visit_template_literal(&mut self, it: &TemplateLiteral<'a>) {
    let it = self.alloc(it);
    if it.expressions.iter().any(|expr| self.unclean(expr)) {
      self.bail(it.span, "template may format an Effect runtime object");
    }
    walk_js::walk_template_literal(self, it);
  }

  fn visit_binary_expression(&mut self, it: &BinaryExpression<'a>) {
    let it = self.alloc(it);
    let formats = match it.operator {
      BinaryOperator::Addition => self.unclean(&it.left) || self.unclean(&it.right),
      BinaryOperator::LessThan
      | BinaryOperator::LessEqualThan
      | BinaryOperator::GreaterThan
      | BinaryOperator::GreaterEqualThan
      | BinaryOperator::Equality
      | BinaryOperator::Inequality => {
        let left_numeric = self.prov.key_numeric(&it.left);
        let right_numeric = self.prov.key_numeric(&it.right);
        (!right_numeric && self.unclean(&it.left)) || (!left_numeric && self.unclean(&it.right))
      }
      // `"name" in value` observes whether a service has that method.
      BinaryOperator::In if self.unclean(&it.right) => {
        match self.prov.shared.const_string(self.prov.module, &it.left) {
          Some(name) => {
            self.names.push(name);
            false
          }
          None => true,
        }
      }
      _ => false,
    };
    if formats {
      self.bail(it.span, "operator may read an Effect runtime object");
    }
    walk_js::walk_binary_expression(self, it);
  }

  fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
    let it = self.alloc(it);
    if it.operator == AssignmentOperator::Addition {
      let target_unclean = match &it.left {
        AssignmentTarget::AssignmentTargetIdentifier(ident) => {
          self.prov.symbol(ident).is_none_or(|symbol| !self.prov.symbol_clean(symbol))
        }
        AssignmentTarget::StaticMemberExpression(member) => self.unclean(&member.object),
        AssignmentTarget::ComputedMemberExpression(member) => self.unclean(&member.object),
        _ => true,
      };
      if target_unclean || self.unclean(&it.right) {
        self.bail(it.span, "`+=` may format an Effect runtime object");
      }
    }
    walk_js::walk_assignment_expression(self, it);
  }

  fn visit_throw_statement(&mut self, it: &ThrowStatement<'a>) {
    let it = self.alloc(it);
    if self.unclean(&it.argument) {
      self.bail(it.span, "thrown value may hold an Effect runtime object");
    }
    walk_js::walk_throw_statement(self, it);
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
            let bare = name.strip_prefix("node:").unwrap_or(name);
            let builtin = NODE_BUILTINS.contains(&bare) || name.starts_with("bun:");
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
      const_strings: FxHashMap::default(),
    };

    // == pass A: keys
    let normal_modules: Vec<ModuleIdx> =
      self.module_table.modules.iter().filter_map(|m| m.as_normal().map(|n| n.idx)).collect();
    let mut key_creations = vec![];
    let mut this_free = vec![];
    let mut const_strings: Vec<(SymbolRef, Vec<StringPart>)> = vec![];
    for module in &normal_modules {
      let Some(ast) = self.ast_table[*module].as_ref() else { continue };
      ast.program.with_dependent(|_owner, dep| {
        let mut collector = KeyCollector {
          shared: &shared,
          module: *module,
          keys: vec![],
          this_free: vec![],
          const_strings: vec![],
          depth: 0,
          kind: VariableDeclarationKind::Var,
        };
        collector.visit_program(&dep.program);
        key_creations.extend(collector.keys.into_iter().map(|key| (*module, key)));
        this_free.extend(collector.this_free);
        const_strings.extend(collector.const_strings);
      });
    }
    for symbol in this_free {
      shared.this_free.insert(shared.symbols.canonical_ref_for(symbol));
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
        report.global_bailouts.push(format!("service key with a dynamic key string ({module:?})"));
      }
    }

    // == pass B: uses
    let mut sites: Vec<Site> = vec![];
    let mut consumers = vec![];
    let mut providers = vec![];
    let mut guard_bindings = vec![];
    let mut refs: FxHashMap<ModuleIdx, FxHashMap<ReferenceId, RefInfo>> = FxHashMap::default();
    let mut namespace_refs: Vec<(SymbolRef, RefInfo)> = vec![];
    let mut key_bailouts = vec![];
    let mut name_reads: Vec<(ModuleIdx, NameRead)> = vec![];
    let mut incomplete_keys: FxHashSet<String> = FxHashSet::default();
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
          namespace_refs: vec![],
          classified: FxHashSet::default(),
          site_spans: FxHashMap::default(),
          sites: &mut sites,
          consumers: vec![],
          providers: vec![],
          guard_bindings: vec![],
          global_bailouts: vec![],
          key_bailouts: vec![],
          name_reads: vec![],
          pattern_source: None,
          incomplete_keys: FxHashSet::default(),
        };
        collector.visit_program(&dep.program);
        name_reads.extend(collector.name_reads.into_iter().map(|read| (*module, read)));
        incomplete_keys.extend(collector.incomplete_keys);
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
        }
      }
    }

    // == provenance of values in untrusted code
    let untrusted: Vec<ModuleIdx> = normal_modules
      .iter()
      .filter(|module| shared.prefixes[module.index()].is_none() && **module != self.runtime.id())
      .copied()
      .collect();
    let mut module_consumers: FxHashMap<ModuleIdx, FxHashMap<SymbolId, String>> =
      FxHashMap::default();
    let mut tracked: FxHashMap<ModuleIdx, FxHashSet<SymbolId>> = FxHashMap::default();
    for (_, consumer) in &consumers {
      if let ConsumerKind::Binding(symbol) = consumer.kind {
        module_consumers
          .entry(symbol.owner)
          .or_default()
          .insert(symbol.symbol, consumer.key.clone());
        tracked.entry(symbol.owner).or_default().insert(symbol.symbol);
      }
    }
    for site in &sites {
      for binding in &site.bindings {
        tracked.entry(binding.owner).or_default().insert(binding.symbol);
      }
    }
    let user_keys: FxHashSet<&String> = shared
      .keys
      .iter()
      .filter(|(symbol, _)| untrusted.contains(&symbol.owner))
      .map(|(_, key)| key)
      .collect();
    let empty_consumers = FxHashMap::default();
    let empty_facts: FxHashMap<String, FxHashMap<String, bool>> = FxHashMap::default();
    // Method results of user providers, for consumers that format them.
    let compute_facts = |heap_tainted: bool| {
      let mut site_facts: FxHashMap<(usize, String), bool> = FxHashMap::default();
      for module in &untrusted {
        let starts: FxHashMap<u32, usize> = sites
          .iter()
          .enumerate()
          .filter(|(_, site)| site.module == *module)
          .map(|(index, site)| (site.start, index))
          .collect();
        if starts.is_empty() {
          continue;
        }
        let Some(ast) = self.ast_table[*module].as_ref() else { continue };
        ast.program.with_dependent(|_owner, dep| {
          let flow = collect_flow(&shared, *module, &dep.program);
          let mut prov =
            Prov::new(&shared, *module, &flow, heap_tainted, &empty_facts, &empty_consumers);
          let mut collector = FactCollector { prov: &mut prov, sites: &starts, facts: vec![] };
          collector.visit_program(&dep.program);
          for (site, name, clean) in collector.facts {
            site_facts.insert((site, name), clean);
          }
        });
      }
      let mut facts: FxHashMap<String, FxHashMap<String, bool>> = FxHashMap::default();
      for key in &user_keys {
        if incomplete_keys.contains(*key) {
          continue;
        }
        let key_sites: Vec<usize> =
          (0..sites.len()).filter(|index| sites[*index].keys.contains(*key)).collect();
        if key_sites.is_empty() {
          continue;
        }
        let names: FxHashSet<&String> = site_facts
          .keys()
          .filter(|(site, _)| key_sites.contains(site))
          .map(|(_, name)| name)
          .collect();
        let methods = names
          .into_iter()
          .map(|name| {
            let clean = key_sites
              .iter()
              .all(|site| site_facts.get(&(*site, name.clone())).copied().unwrap_or(false));
            (name.clone(), clean)
          })
          .collect();
        facts.insert((*key).clone(), methods);
      }
      facts
    };
    // Whether any object or collection in user code may hold a runtime value,
    // assuming none does: every write must keep that true.
    let mut facts = compute_facts(false);
    let mut heap_tainted = false;
    for module in &untrusted {
      let Some(ast) = self.ast_table[*module].as_ref() else { continue };
      ast.program.with_dependent(|_owner, dep| {
        let flow = collect_flow(&shared, *module, &dep.program);
        let consumers = module_consumers.get(module).unwrap_or(&empty_consumers);
        let mut prov = Prov::new(&shared, *module, &flow, false, &facts, consumers);
        heap_tainted |=
          flow.heap_unknown || flow.heap_writes.iter().any(|value| !prov.clean(value));
      });
      if heap_tainted {
        break;
      }
    }
    if heap_tainted {
      facts = compute_facts(true);
    }
    let mut unknown_reads: Vec<(String, Vec<Guard>)> = vec![];
    for module in &untrusted {
      let Some(ast) = self.ast_table[*module].as_ref() else { continue };
      let receivers: FxHashSet<(u32, u32)> = name_reads
        .iter()
        .filter(|(owner, _)| owner == module)
        .filter_map(|(_, (receiver, _, _))| *receiver)
        .collect();
      let empty_tracked = FxHashSet::default();
      let module_tracked = tracked.get(module).unwrap_or(&empty_tracked);
      ast.program.with_dependent(|_owner, dep| {
        let flow = collect_flow(&shared, *module, &dep.program);
        let consumers = module_consumers.get(module).unwrap_or(&empty_consumers);
        let mut prov = Prov::new(&shared, *module, &flow, heap_tainted, &facts, consumers);
        let mut checker = SinkChecker {
          prov: &mut prov,
          path: vec![],
          receivers: &receivers,
          tracked: module_tracked,
          unclean_receivers: FxHashSet::default(),
          names: vec![],
          global_bailouts: vec![],
        };
        checker.visit_program(&dep.program);
        report.global_bailouts.extend(checker.global_bailouts);
        unknown_reads.extend(checker.names.into_iter().map(|name| (name, vec![])));
        for (owner, (receiver, name, chain)) in &name_reads {
          if owner != module {
            continue;
          }
          if receiver.is_none_or(|span| checker.unclean_receivers.contains(&span)) {
            unknown_reads.push((name.clone(), chain.clone()));
          }
        }
      });
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
      // A read on a value that may hold a service demands the name on every service.
      for (name, chain) in &unknown_reads {
        if live(&site_demand, &live_bindings, chain) {
          for demand in &mut site_demand {
            changed |= demand.add(name);
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
