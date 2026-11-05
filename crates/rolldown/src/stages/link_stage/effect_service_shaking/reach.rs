//! Which values of the analysed code may reach an Effect runtime object that
//! holds services: a Context, a Fiber, a runtime, or a service taken from
//! one without a key.
//!
//! Services leave a Context through a keyed lookup, which the pass tracks per
//! key, or through such an object. A call of an Effect API is first evaluated
//! through the API's own definition in Effect's code (see "Effect's own code"
//! below), where only the primitives the fiber interprets are modelled: a
//! `withFiber` closure receives the fiber, whose `context` holds every
//! service. When the evaluator does not cover a definition yet, the call
//! falls back to the API tables: the sources (`fork*`, `runFork`,
//! `Layer.build`, `ManagedRuntime.make`, callbacks receiving a fiber, ...),
//! a few precise summaries, and, for every other API, passing on what it was
//! given. Members of the global object other than builtins are unknown values.
//!
//! The analysis is a flow-insensitive abstract interpretation of the code
//! outside Effect's own modules. A value is three bits: it may be or contain a
//! runtime object; the value it succeeds with (an Effect, a Promise or a
//! Fiber) may; the success of that success may. Containers are not split by
//! field. Functions are followed through direct calls, Effect callbacks and,
//! for method calls, every function defined under the method's name.
//!
//! What can observe a service through such a value:
//! - a static read `x.name` demands `name` of every service;
//! - a read by a name computed at run time, reflection (`Object.keys`, spread,
//!   `for...in`, `JSON.stringify`, string conversion, ...) or a call into code
//!   the analysis cannot see keeps every method of every service.

use oxc::{
  allocator::Vec as ArenaVec,
  ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, ArrowFunctionExpression, AssignmentTarget,
    AssignmentTargetMaybeDefault, AssignmentTargetProperty, BinaryOperator, BindingPattern,
    CallExpression, CatchClause, ChainElement, Class, ClassElement, Expression, ForInStatement,
    ForOfStatement, ForStatementLeft, FormalParameters, Function, MethodDefinitionKind,
    ObjectPropertyKind, Program, PropertyKey, PropertyKind, ReturnStatement,
    SimpleAssignmentTarget, Statement, ThrowStatement, UnaryOperator, VariableDeclarator,
    WithStatement,
  },
  ast_visit::{VisitJs, walk_js},
  semantic::ScopeFlags,
  span::{GetSpan, Span},
};
use rolldown_common::{ModuleIdx, SymbolRef};
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use super::{GLOBAL_OBJECTS, Guard, Resolved, Shared, strip};

/// Effect APIs whose result is or holds a runtime object (effect 4.1.2).
const RESULT_SOURCES: &[&str] = &[
  "Effect.context",
  "Effect.fiber",
  "Effect.withFiber",
  "Effect.withFiberSucceed",
  "Effect.contextWith",
  "Effect.runCallback",
  "Effect.runCallbackWith",
  "Fiber.getCurrent",
  "Layer.build",
  "Layer.buildWithScope",
  "Layer.buildWithMemoMap",
  "Layer.makeMemoMap",
  "Layer.makeMemoMapUnsafe",
  "Layer.forkMemoMap",
  "Layer.forkMemoMapUnsafe",
  "ManagedRuntime.make",
];
/// Effect APIs that call a callback with a runtime object (effect 4.1.2):
/// the Context (`contextWith`, `updateContext`, `Layer.flatMap`), the fiber
/// (`withFiber`, `Logger.make`, `Runtime.makeRunMain`, race `onWinner`,
/// `ErrorReporter.make`), request entries holding their Context
/// (`RequestResolver`) or the tracer's `context` hook (`Tracer.make`).
const CALLBACK_SOURCES: &[&str] = &[
  "Effect.withFiber",
  "Effect.withFiberSucceed",
  "Effect.contextWith",
  "Effect.updateContext",
  "Effect.race",
  "Effect.raceFirst",
  "Effect.raceAll",
  "Effect.raceAllFirst",
  "Effect.runCallback",
  "Effect.runCallbackWith",
  "Layer.flatMap",
  "Layer.tap",
  "Layer.fromBuild",
  "Layer.fromBuildMemo",
  "Logger.make",
  "Tracer.make",
  "Runtime.makeRunMain",
  "ErrorReporter.make",
];
/// Whole modules whose values hold fibers or Contexts.
const SOURCE_MODULES: &[&str] = &[
  "FiberHandle.",
  "FiberMap.",
  "FiberSet.",
  "RequestResolver.",
  "Cache.",
  "RcMap.",
  "LayerMap.",
  "LayerRef.",
];
/// Methods that run an Effect, on a `ManagedRuntime` or any other receiver.
const RUNNER_METHODS: &[&str] =
  &["runPromise", "runPromiseExit", "runSync", "runSyncExit", "runFork", "runCallback"];
/// Global values that hold no runtime object.
const BUILTIN_GLOBALS: &[&str] = &[
  "Object",
  "Array",
  "JSON",
  "Math",
  "Reflect",
  "Promise",
  "Symbol",
  "Map",
  "Set",
  "WeakMap",
  "WeakSet",
  "WeakRef",
  "FinalizationRegistry",
  "Proxy",
  "Error",
  "TypeError",
  "RangeError",
  "SyntaxError",
  "ReferenceError",
  "EvalError",
  "URIError",
  "AggregateError",
  "Date",
  "RegExp",
  "Number",
  "String",
  "Boolean",
  "BigInt",
  "parseInt",
  "parseFloat",
  "isNaN",
  "isFinite",
  "encodeURI",
  "encodeURIComponent",
  "decodeURI",
  "decodeURIComponent",
  "escape",
  "unescape",
  "console",
  "setTimeout",
  "clearTimeout",
  "setInterval",
  "clearInterval",
  "setImmediate",
  "clearImmediate",
  "queueMicrotask",
  "structuredClone",
  "process",
  "Buffer",
  "URL",
  "URLSearchParams",
  "TextEncoder",
  "TextDecoder",
  "fetch",
  "Request",
  "Response",
  "Headers",
  "FormData",
  "Blob",
  "File",
  "AbortController",
  "AbortSignal",
  "Event",
  "EventTarget",
  "performance",
  "crypto",
  "atob",
  "btoa",
  "Intl",
  "ArrayBuffer",
  "SharedArrayBuffer",
  "DataView",
  "Int8Array",
  "Uint8Array",
  "Uint8ClampedArray",
  "Int16Array",
  "Uint16Array",
  "Int32Array",
  "Uint32Array",
  "Float32Array",
  "Float64Array",
  "BigInt64Array",
  "BigUint64Array",
  "Atomics",
  "Iterator",
  "ReadableStream",
  "WritableStream",
  "TransformStream",
  "MessageChannel",
  "BroadcastChannel",
  "WebSocket",
  "navigator",
  "undefined",
  "NaN",
  "Infinity",
];
/// Static functions of builtins that only move values, never reading a
/// property by a name the program cannot see or converting to a string.
const TRANSFER_STATICS: &[&str] = &[
  "Object.freeze",
  "Object.seal",
  "Object.preventExtensions",
  "Object.isFrozen",
  "Object.isSealed",
  "Object.isExtensible",
  "Object.getPrototypeOf",
  "Object.is",
  "Object.create",
  "Object.fromEntries",
  "Array.from",
  "Array.of",
  "Array.isArray",
  "Promise.resolve",
  "Promise.reject",
  "Promise.all",
  "Promise.allSettled",
  "Promise.race",
  "Promise.any",
  "Promise.withResolvers",
  "Reflect.getPrototypeOf",
  "Reflect.isExtensible",
  "Boolean",
  "queueMicrotask",
  "setTimeout",
  "setInterval",
  "setImmediate",
  "clearTimeout",
  "clearInterval",
  "clearImmediate",
  "Symbol.for",
];
/// Methods that do not call functions other than their callback arguments.
const NONCALLING_METHODS: &[&str] = &[
  "push",
  "pop",
  "shift",
  "unshift",
  "splice",
  "slice",
  "set",
  "get",
  "has",
  "add",
  "delete",
  "clear",
  "keys",
  "values",
  "entries",
  "at",
  "fill",
  "flat",
  "reverse",
  "copyWithin",
  "map",
  "forEach",
  "filter",
  "find",
  "findIndex",
  "findLast",
  "findLastIndex",
  "some",
  "every",
  "reduce",
  "reduceRight",
  "flatMap",
  "then",
  "catch",
  "finally",
  "next",
  "of",
  "pipe",
];
/// Methods that convert their receiver or arguments to strings or numbers,
/// which formats a runtime object and the services it holds.
const COERCING_METHODS: &[&str] = &[
  "concat",
  "includes",
  "indexOf",
  "lastIndexOf",
  "startsWith",
  "endsWith",
  "split",
  "replace",
  "replaceAll",
  "match",
  "matchAll",
  "search",
  "localeCompare",
  "padStart",
  "padEnd",
  "join",
  "sort",
  "toSorted",
  "toLocaleString",
  "toString",
  "valueOf",
  "toJSON",
  "append",
  "set",
  "write",
  "end",
  "send",
  "log",
  "info",
  "warn",
  "error",
  "debug",
  "trace",
  "dir",
  "table",
  "assert",
  "format",
  "inspect",
];
/// Methods that, called on a runtime object, read every service it holds.
const RECEIVER_REFLECTIVE_METHODS: &[&str] = &[
  "toString",
  "toLocaleString",
  "toJSON",
  "valueOf",
  "join",
  "sort",
  "toSorted",
  "constructor",
  "hasOwnProperty",
  "propertyIsEnumerable",
  "__lookupGetter__",
  "__lookupSetter__",
  "__defineGetter__",
  "__defineSetter__",
];
/// Members that hold numbers on builtins and on Effect's runtime objects:
/// reading one from a runtime object yields only what the analysed code
/// stores under that name itself.
const PRIMITIVE_MEMBERS: &[&str] = &["length", "size", "byteLength"];
/// Names of methods and accessors on builtin prototypes.
const BUILTIN_MEMBERS: &[&str] = &[
  "constructor",
  "toString",
  "toLocaleString",
  "valueOf",
  "hasOwnProperty",
  "isPrototypeOf",
  "propertyIsEnumerable",
  "__proto__",
  "length",
  "at",
  "concat",
  "copyWithin",
  "entries",
  "every",
  "fill",
  "filter",
  "find",
  "findIndex",
  "findLast",
  "findLastIndex",
  "flat",
  "flatMap",
  "forEach",
  "includes",
  "indexOf",
  "join",
  "keys",
  "lastIndexOf",
  "map",
  "pop",
  "push",
  "reduce",
  "reduceRight",
  "reverse",
  "shift",
  "slice",
  "some",
  "sort",
  "splice",
  "toReversed",
  "toSorted",
  "toSpliced",
  "unshift",
  "values",
  "with",
  "get",
  "set",
  "has",
  "add",
  "delete",
  "clear",
  "size",
  "then",
  "catch",
  "finally",
  "next",
  "return",
  "throw",
  "call",
  "apply",
  "bind",
  "name",
  "charAt",
  "charCodeAt",
  "codePointAt",
  "endsWith",
  "localeCompare",
  "match",
  "matchAll",
  "normalize",
  "padEnd",
  "padStart",
  "repeat",
  "replace",
  "replaceAll",
  "search",
  "split",
  "startsWith",
  "substring",
  "substr",
  "toLowerCase",
  "toUpperCase",
  "trim",
  "trimStart",
  "trimEnd",
  "toFixed",
  "toPrecision",
  "toExponential",
  "getTime",
  "toISOString",
  "toJSON",
  "message",
  "stack",
  "cause",
  "description",
  "source",
  "flags",
  "test",
  "exec",
  "buffer",
  "byteLength",
  "byteOffset",
  "subarray",
  "deref",
  "register",
  "unregister",
  "addEventListener",
  "removeEventListener",
  "dispatchEvent",
  "abort",
  "signal",
  "aborted",
  "reason",
  "json",
  "text",
  "arrayBuffer",
  "body",
  "headers",
  "status",
  "ok",
  "url",
  "append",
];

/// A function: module and span start of its node.
type FnId = (ModuleIdx, u32);

/// An Effect API and the reach and functions of each argument.
type DeriveKey = (SymbolRef, Vec<(u8, Vec<FnId>)>);

/// What a read of a runtime object observes, with the guards around the read
/// and its module and offset.
pub(super) type ReachEvents = Vec<(ReachEvent, Vec<Guard>, ModuleIdx, u32)>;

/// What a value may reach: bit 0, the value itself; bit 1, what it succeeds
/// with when run, awaited or joined; bit 2, the success of that success.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Reach(u8);

impl Reach {
  const NONE: Reach = Reach(0);
  const TOP: Reach = Reach(0b111);

  fn value(self) -> bool {
    self.0 & 1 != 0
  }
  fn any(self) -> bool {
    self.0 != 0
  }
  fn join(self, other: Reach) -> Reach {
    Reach(self.0 | other.0)
  }
  /// Anything reachable at any depth, at every depth.
  fn collapse(self) -> Reach {
    if self.any() { Reach::TOP } else { Reach::NONE }
  }
  /// What running, awaiting or joining the value yields.
  fn success(self) -> Reach {
    if self.value() {
      return Reach::TOP;
    }
    let s1 = self.0 & 0b010 != 0;
    let s2 = self.0 & 0b100 != 0;
    Reach(u8::from(s1) | (u8::from(s2) << 1) | (u8::from(s2) << 2))
  }
  /// An Effect or Promise that succeeds with `self`.
  fn effect_of(self) -> Reach {
    let deeper = self.0 & 0b110 != 0;
    Reach(((self.0 & 1) << 1) | (u8::from(deeper) << 2))
  }
  /// A Fiber running an Effect `self`: itself a runtime object.
  fn fiber_of(self) -> Reach {
    Reach(1 | (self.0 & 0b110))
  }
  /// What joining a Fiber (or a container of Fibers) yields.
  fn fiber_success(self) -> Reach {
    let s1 = self.0 & 0b010 != 0;
    let s2 = self.0 & 0b100 != 0;
    Reach(u8::from(s1) | (u8::from(s2) << 1) | (u8::from(s2) << 2))
  }
  /// A named property of the value: the value itself for containers,
  /// anything for a runtime object.
  fn member(self) -> Reach {
    if self.value() { Reach::TOP } else { self }
  }
  /// An indexed or iterated element of the value. Runtime objects (Context,
  /// Fiber, runtime) have no indexed members of their own, and what is
  /// reached through a named member is already anything, so the container's
  /// value covers its elements.
  fn element(self) -> Reach {
    self
  }
  fn awaited(self) -> Reach {
    self.join(self.success())
  }
}

/// What a read of a runtime object observes.
#[derive(Clone, Debug)]
pub(super) enum ReachEvent {
  /// A property by this name.
  Name(String),
  /// Properties named by canonical array indices.
  Numeric,
  /// Every property.
  All(&'static str),
}

/// Functions, methods by name and constructors of the analysed modules.
#[derive(Default)]
struct ReachInfo {
  /// Bindings never reassigned that hold a function: the function and
  /// whether calling the binding returns an Effect running it (`Effect.fn`).
  fn_of_symbol: FxHashMap<SymbolRef, (FnId, bool)>,
  /// Functions defined as a property, method or field by this name.
  methods: FxHashMap<String, Vec<(FnId, bool)>>,
  getters: FxHashMap<String, Vec<FnId>>,
  setters: FxHashMap<String, Vec<FnId>>,
  /// Class bindings: their own constructor and what they extend.
  classes: FxHashMap<SymbolRef, (Option<FnId>, Heritage)>,
  /// Property names defined anywhere in Effect's own bundled modules.
  effect_names: FxHashSet<String>,
  async_fns: FxHashSet<FnId>,
  generator_fns: FxHashSet<FnId>,
  /// Variables initialised with a number literal.
  counters: FxHashSet<(ModuleIdx, oxc::semantic::SymbolId)>,
  /// Generators run by `Effect.gen` or `Effect.fn`: `yield*` gives the success.
  effect_generators: FxHashSet<FnId>,
  /// `const` bindings never reassigned, holding data or a value an Effect
  /// API built.
  const_kinds: FxHashMap<SymbolRef, ArgKind>,
}

#[derive(Default)]
struct ReachState {
  bindings: FxHashMap<SymbolRef, Reach>,
  params: FxHashMap<FnId, Vec<Reach>>,
  rest: FxHashMap<FnId, Reach>,
  rets: FxHashMap<FnId, Reach>,
  escaped: FxHashSet<FnId>,
  /// Arguments of calls into code the analysis cannot see, which may be any
  /// escaped function.
  esc_params: Reach,
  /// Returns of every escaped function.
  esc_ret: Reach,
  /// Receivers of method calls.
  this_value: Reach,
  /// Thrown and failed values, and errors Effect builds around a fiber.
  thrown: Reach,
  /// A runtime object was stored in a global.
  global_value: Reach,
  /// A builtin or external function was used as a value, so a callee the
  /// analysis cannot identify may be one.
  opaque_fn: bool,
  /// Values stored under each property name by the analysed code.
  props: FxHashMap<String, Reach>,
  /// Effect APIs whose calls were derived from their definitions, and those
  /// that fell back to the API tables, for the report.
  derived: FxHashSet<String>,
  underived: FxHashSet<String>,
  /// Derived results of this iteration, and derivations that failed, by API
  /// and argument shape.
  derive_cache: FxHashMap<DeriveKey, Option<Reach>>,
  derive_failures: FxHashSet<DeriveKey>,
  /// The last walk records events: derivations run again.
  final_walk: bool,
  /// Where each global fact first grew, for debugging.
  origins: Vec<(&'static str, ModuleIdx, u32)>,
  changed: bool,
}

impl ReachState {
  fn join_binding(&mut self, symbol: SymbolRef, value: Reach) {
    let entry = self.bindings.entry(symbol).or_default();
    let joined = entry.join(value);
    if joined != *entry {
      *entry = joined;
      self.changed = true;
    }
  }
  fn join_param(&mut self, function: FnId, index: usize, value: Reach) {
    let params = self.params.entry(function).or_default();
    if params.len() <= index {
      params.resize(index + 1, Reach::NONE);
    }
    let joined = params[index].join(value);
    if joined != params[index] {
      params[index] = joined;
      self.changed = true;
    }
  }
  fn join_rest(&mut self, function: FnId, value: Reach) {
    let entry = self.rest.entry(function).or_default();
    let joined = entry.join(value);
    if joined != *entry {
      *entry = joined;
      self.changed = true;
    }
  }
  fn join_ret(&mut self, function: FnId, value: Reach) {
    let entry = self.rets.entry(function).or_default();
    let joined = entry.join(value);
    if joined != *entry {
      *entry = joined;
      self.changed = true;
    }
  }
  fn join_field(field: &mut Reach, value: Reach, changed: &mut bool) {
    let joined = field.join(value);
    if joined != *field {
      *field = joined;
      *changed = true;
    }
  }
  fn join_prop(&mut self, name: &str, value: Reach) {
    if !value.any() {
      return;
    }
    let entry = self.props.entry(name.to_string()).or_default();
    let joined = entry.join(value);
    if joined != *entry {
      *entry = joined;
      self.changed = true;
    }
  }
  fn escape(&mut self, function: FnId) {
    if self.escaped.insert(function) {
      self.changed = true;
    }
  }
  fn ret(&self, function: FnId) -> Reach {
    self.rets.get(&function).copied().unwrap_or_default()
  }
}

/// Output of the analysis: what reads of runtime objects observe, with the
/// guards around each read, and where every property was kept.
pub(super) struct ReachOutcome {
  pub(super) events: ReachEvents,
}

pub(super) struct ReachInput<'s, 'l, 'a> {
  pub(super) shared: &'s Shared<'l>,
  /// The analysed modules and their programs.
  pub(super) programs: Vec<(ModuleIdx, &'s Program<'a>)>,
  /// Effect's own bundled modules.
  pub(super) effect_programs: Vec<(ModuleIdx, &'s Program<'a>)>,
  pub(super) site_spans: &'s FxHashMap<(ModuleIdx, u32), usize>,
  pub(super) guard_declarators: &'s FxHashMap<(ModuleIdx, u32), SymbolRef>,
  /// Modules outside the bundle.
  pub(super) externals: FxHashSet<ModuleIdx>,
}

pub(super) fn analyse(input: &ReachInput<'_, '_, '_>) -> ReachOutcome {
  let started = std::time::Instant::now();
  let mut info = ReachInfo::default();
  let mut defs = EffectDefs::default();
  for (module, program) in &input.effect_programs {
    let mut names = NameCollector { names: &mut info.effect_names };
    names.visit_program(program);
    defs.collect(input.shared, *module, program);
  }
  defs.find_keys(input.shared);
  // Function bindings first, then properties whose value is one of them.
  let mut pending_props: Vec<(String, SymbolRef)> = vec![];
  for (module, program) in &input.programs {
    let mut collector = InfoCollector {
      shared: input.shared,
      module: *module,
      info: &mut info,
      pending_props: &mut pending_props,
    };
    collector.visit_program(program);
  }
  for (name, symbol) in pending_props {
    if let Some(entry) = info.fn_of_symbol.get(&symbol).copied() {
      info.methods.entry(name).or_default().push(entry);
    }
  }

  #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
  if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_DEBUG").is_some() {
    eprintln!("[reach] setup {} ms", started.elapsed().as_millis());
  }
  let mut state = ReachState::default();
  // Flow-insensitive: walk everything until no value grows.
  for _ in 0..64 {
    state.changed = false;
    state.derive_cache.clear();
    for (module, program) in &input.programs {
      let mut walker = Walker::new(input, &info, &defs, &mut state, *module, None);
      walker.visit_program(program);
    }
    let mut esc_ret = Reach::NONE;
    for function in &state.escaped {
      esc_ret = esc_ret.join(state.ret(*function));
    }
    let mut changed = state.changed;
    ReachState::join_field(&mut state.esc_ret, esc_ret, &mut changed);
    if !changed {
      break;
    }
  }
  #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
  if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_DEBUG").is_some() {
    let mut derived: Vec<_> = state.derived.iter().collect();
    derived.sort();
    let mut underived: Vec<_> = state.underived.difference(&state.derived).collect();
    underived.sort();
    eprintln!(
      "[reach] {} ms, {} derivations cached, {} failed",
      started.elapsed().as_millis(),
      state.derive_cache.len(),
      state.derive_failures.len()
    );
    eprintln!("[reach] derived from definitions: {derived:?}");
    eprintln!("[reach] from the API tables: {underived:?}");
    eprintln!(
      "[reach] thrown={:?} esc_params={:?} esc_ret={:?} this={:?} global={:?} escaped={}",
      state.thrown,
      state.esc_params,
      state.esc_ret,
      state.this_value,
      state.global_value,
      state.escaped.len()
    );
    for (what, module, start) in &state.origins {
      let source =
        input.programs.iter().find(|(m, _)| m == module).map_or("", |(_, p)| p.source_text);
      let start = *start as usize;
      let snippet = source.get(start..(start + 240).min(source.len())).unwrap_or("");
      eprintln!("[reach] origin {what} module {module:?} at {start}: {snippet}");
    }
  }
  state.final_walk = true;
  state.derive_cache.clear();
  let mut events = vec![];
  for (module, program) in &input.programs {
    let mut walker = Walker::new(input, &info, &defs, &mut state, *module, Some(&mut events));
    walker.visit_program(program);
  }
  ReachOutcome { events }
}

/// Property names defined in Effect's modules.
struct NameCollector<'n> {
  names: &'n mut FxHashSet<String>,
}

fn key_name(key: &PropertyKey<'_>) -> Option<String> {
  key.static_name().and_then(|name| name.as_js_str().as_str().map(String::from))
}

impl<'a> VisitJs<'a> for NameCollector<'_> {
  fn visit_object_property(&mut self, it: &oxc::ast::ast::ObjectProperty<'a>) {
    if let Some(name) = key_name(&it.key) {
      self.names.insert(name);
    }
    walk_js::walk_object_property(self, it);
  }
  fn visit_method_definition(&mut self, it: &oxc::ast::ast::MethodDefinition<'a>) {
    if let Some(name) = key_name(&it.key) {
      self.names.insert(name);
    }
    walk_js::walk_method_definition(self, it);
  }
  fn visit_property_definition(&mut self, it: &oxc::ast::ast::PropertyDefinition<'a>) {
    if let Some(name) = key_name(&it.key) {
      self.names.insert(name);
    }
    walk_js::walk_property_definition(self, it);
  }
  fn visit_accessor_property(&mut self, it: &oxc::ast::ast::AccessorProperty<'a>) {
    if let Some(name) = key_name(&it.key) {
      self.names.insert(name);
    }
    walk_js::walk_accessor_property(self, it);
  }
  fn visit_assignment_expression(&mut self, it: &oxc::ast::ast::AssignmentExpression<'a>) {
    if let AssignmentTarget::StaticMemberExpression(member) = &it.left {
      self.names.insert(member.property.name.to_string());
    }
    walk_js::walk_assignment_expression(self, it);
  }
  fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
    // `Object.defineProperty(target, "name", ...)`.
    if let Expression::StaticMemberExpression(member) = strip(&it.callee) {
      if member.property.name == "defineProperty" {
        if let Some(Expression::StringLiteral(name)) =
          it.arguments.get(1).and_then(Argument::as_expression)
        {
          if let Some(name) = name.value.as_str() {
            self.names.insert(name.to_string());
          }
        }
      }
    }
    walk_js::walk_call_expression(self, it);
  }
}

struct InfoCollector<'s, 'l, 'i> {
  shared: &'s Shared<'l>,
  module: ModuleIdx,
  info: &'i mut ReachInfo,
  pending_props: &'i mut Vec<(String, SymbolRef)>,
}

impl InfoCollector<'_, '_, '_> {
  /// The function an `Effect.fn` call runs: `Effect.fn(gen, ...)`,
  /// `Effect.fn("name")(gen, ...)` or `Effect.fn("name", options)(gen)`.
  fn effect_fn(&self, expr: &Expression<'_>) -> Option<FnId> {
    effect_fn_generator(self.shared, self.module, expr)
  }

  fn function_value(&self, expr: &Expression<'_>) -> Option<(FnId, bool)> {
    match strip(expr) {
      Expression::FunctionExpression(function) => Some(((self.module, function.span.start), false)),
      Expression::ArrowFunctionExpression(arrow) => Some(((self.module, arrow.span.start), false)),
      other => self.effect_fn(other).map(|function| (function, true)),
    }
  }

  fn heritage(&self, class: &Class<'_>) -> Heritage {
    let Some(heritage) = &class.heritage else { return Heritage::None };
    let mut expr = strip(&heritage.expression);
    // `Context.Service<..>()("key")`, `Schema.Class<..>("Id")({...})`.
    while let Expression::CallExpression(call) = expr {
      expr = strip(&call.callee);
    }
    if self.shared.api_of(self.module, expr).is_some() {
      return Heritage::Effect;
    }
    match self.shared.resolve_expr(self.module, expr) {
      Some(Resolved::Symbol(symbol)) if self.shared.keys.contains_key(&symbol) => Heritage::Effect,
      Some(Resolved::Symbol(symbol)) => Heritage::Class(symbol),
      _ => Heritage::Unknown,
    }
  }

  fn symbol_of(&self, id: &oxc::ast::ast::BindingIdentifier<'_>) -> Option<SymbolRef> {
    let symbol = id.symbol_id.get()?;
    Some(self.shared.symbols.canonical_ref_for((self.module, symbol).into()))
  }
}

/// The generator function of an `Effect.fn` / `Effect.fnUntraced` call.
fn effect_fn_generator(
  shared: &Shared<'_>,
  module: ModuleIdx,
  expr: &Expression<'_>,
) -> Option<FnId> {
  let Expression::CallExpression(call) = strip(expr) else { return None };
  effect_fn_call_generator(shared, module, call)
}

fn effect_fn_call_generator(
  shared: &Shared<'_>,
  module: ModuleIdx,
  call: &CallExpression<'_>,
) -> Option<FnId> {
  let is_fn = |callee: &Expression<'_>| {
    matches!(shared.api_of(module, callee), Some("Effect.fn" | "Effect.fnUntraced"))
  };
  let generator = |arguments: &ArenaVec<'_, Argument<'_>>| {
    arguments.iter().find_map(|argument| match argument.as_expression().map(strip) {
      Some(Expression::FunctionExpression(function)) if function.generator => {
        Some((module, function.span.start))
      }
      _ => None,
    })
  };
  if is_fn(&call.callee) {
    return generator(&call.arguments);
  }
  if let Expression::CallExpression(inner) = strip(&call.callee) {
    if is_fn(&inner.callee) {
      return generator(&call.arguments);
    }
  }
  None
}

impl<'a> VisitJs<'a> for InfoCollector<'_, '_, '_> {
  fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
    if matches!(self.shared.api_of(self.module, &it.callee), Some("Effect.gen" | "Stream.gen")) {
      for argument in &it.arguments {
        if let Some(Expression::FunctionExpression(function)) = argument.as_expression().map(strip)
        {
          if function.generator {
            self.info.effect_generators.insert((self.module, function.span.start));
          }
        }
      }
    }
    if let Some(generator) = effect_fn_call_generator(self.shared, self.module, it) {
      self.info.effect_generators.insert(generator);
    }
    walk_js::walk_call_expression(self, it);
  }

  fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
    let id = (self.module, it.span.start);
    if it.r#async {
      self.info.async_fns.insert(id);
    }
    if it.generator {
      self.info.generator_fns.insert(id);
    }
    if let Some(symbol) = it.id.as_ref().and_then(|id| self.symbol_of(id)) {
      if it.is_declaration() {
        self.info.fn_of_symbol.insert(symbol, (id, false));
      }
    }
    walk_js::walk_function(self, it, flags);
  }

  fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
    if it.r#async {
      self.info.async_fns.insert((self.module, it.span.start));
    }
    walk_js::walk_arrow_function_expression(self, it);
  }

  fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
    if let (BindingPattern::BindingIdentifier(id), Some(Expression::NumericLiteral(_))) =
      (&it.id, it.init.as_ref().map(strip))
    {
      if let Some(symbol) = id.symbol_id.get() {
        self.info.counters.insert((self.module, symbol));
      }
    }
    if let (BindingPattern::BindingIdentifier(id), Some(init)) = (&it.id, &it.init) {
      let mutated = id
        .symbol_id
        .get()
        .is_none_or(|symbol| self.shared.scoping(self.module).symbol_is_mutated(symbol));
      if !mutated {
        let kind = match strip(init) {
          Expression::CallExpression(call) => {
            let mut callee = strip(&call.callee);
            while let Expression::CallExpression(inner) = callee {
              callee = strip(&inner.callee);
            }
            self.shared.api_of(self.module, callee).map(|_| ArgKind::Effect)
          }
          Expression::ObjectExpression(_)
          | Expression::ArrayExpression(_)
          | Expression::StringLiteral(_)
          | Expression::NumericLiteral(_)
          | Expression::BooleanLiteral(_)
          | Expression::NullLiteral(_)
          | Expression::TemplateLiteral(_)
          | Expression::ClassExpression(_) => Some(ArgKind::Known),
          other => self.shared.api_of(self.module, other).map(|_| ArgKind::Effect),
        };
        if let (Some(symbol), Some(kind)) = (self.symbol_of(id), kind) {
          self.info.const_kinds.insert(symbol, kind);
        }
      }
    }
    if let (BindingPattern::BindingIdentifier(id), Some(init)) = (&it.id, &it.init) {
      let mutated = id
        .symbol_id
        .get()
        .is_none_or(|symbol| self.shared.scoping(self.module).symbol_is_mutated(symbol));
      if !mutated {
        if let (Some(symbol), Some(function)) = (self.symbol_of(id), self.function_value(init)) {
          self.info.fn_of_symbol.insert(symbol, function);
        }
        if let (Some(symbol), Expression::ClassExpression(class)) =
          (self.symbol_of(id), strip(init))
        {
          let heritage = self.heritage(class);
          self.info.classes.insert(symbol, (class_constructor(self.module, class), heritage));
        }
      }
    }
    walk_js::walk_variable_declarator(self, it);
  }

  fn visit_class(&mut self, it: &Class<'a>) {
    if let Some(symbol) = it.id.as_ref().and_then(|id| self.symbol_of(id)) {
      let heritage = self.heritage(it);
      self.info.classes.insert(symbol, (class_constructor(self.module, it), heritage));
    }
    for element in &it.body.body {
      match element {
        ClassElement::MethodDefinition(method) => {
          let Some(name) = key_name(&method.key) else { continue };
          let id = (self.module, method.value.span.start);
          match method.kind {
            MethodDefinitionKind::Method => {
              self.info.methods.entry(name).or_default().push((id, false));
            }
            MethodDefinitionKind::Get => self.info.getters.entry(name).or_default().push(id),
            MethodDefinitionKind::Set => self.info.setters.entry(name).or_default().push(id),
            MethodDefinitionKind::Constructor => {}
          }
        }
        ClassElement::PropertyDefinition(property) => {
          let (Some(name), Some(value)) = (key_name(&property.key), &property.value) else {
            continue;
          };
          if let Some(function) = self.function_value(value) {
            self.info.methods.entry(name).or_default().push(function);
          }
        }
        _ => {}
      }
    }
    walk_js::walk_class(self, it);
  }

  fn visit_object_property(&mut self, it: &oxc::ast::ast::ObjectProperty<'a>) {
    if let Some(name) = if it.computed { None } else { key_name(&it.key) } {
      match it.kind {
        PropertyKind::Init => match strip(&it.value) {
          Expression::Identifier(ident) => {
            if let Some(Resolved::Symbol(symbol)) = self.shared.resolve_ident(self.module, ident) {
              self.pending_props.push((name, symbol));
            }
          }
          value => {
            if let Some(function) = self.function_value(value) {
              self.info.methods.entry(name).or_default().push(function);
            }
          }
        },
        PropertyKind::Get => {
          if let Expression::FunctionExpression(function) = strip(&it.value) {
            self.info.getters.entry(name).or_default().push((self.module, function.span.start));
          }
        }
        PropertyKind::Set => {
          if let Expression::FunctionExpression(function) = strip(&it.value) {
            self.info.setters.entry(name).or_default().push((self.module, function.span.start));
          }
        }
      }
    }
    walk_js::walk_object_property(self, it);
  }

  fn visit_assignment_expression(&mut self, it: &oxc::ast::ast::AssignmentExpression<'a>) {
    if let AssignmentTarget::StaticMemberExpression(member) = &it.left {
      if let Some(function) = self.function_value(&it.right) {
        self.info.methods.entry(member.property.name.to_string()).or_default().push(function);
      }
    }
    walk_js::walk_assignment_expression(self, it);
  }
}

fn class_constructor(module: ModuleIdx, class: &Class<'_>) -> Option<FnId> {
  class.body.body.iter().find_map(|element| match element {
    ClassElement::MethodDefinition(method) if method.kind == MethodDefinitionKind::Constructor => {
      Some((module, method.value.span.start))
    }
    _ => None,
  })
}

/// What a class extends.
#[derive(Clone, Copy)]
enum Heritage {
  None,
  /// An Effect class such as `Data.Error` or a key class: it stores what it is
  /// given.
  Effect,
  Class(SymbolRef),
  Unknown,
}

/// An argument after evaluation.
struct Arg {
  value: Reach,
  spread: bool,
  /// Functions passed directly as this argument, or inside an object or
  /// array literal passed as it.
  functions: Vec<FnId>,
  kind: ArgKind,
}

/// What calling an argument back runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArgKind {
  /// Data, or functions the analysis follows (`functions`).
  Known,
  /// A keyed lookup by tracked keys, such as `Context.get(Key)`: the
  /// service it returns is followed per key.
  Lookup,
  /// An Effect API or a value an Effect API built: Effect's own code.
  Effect,
  /// A builtin or external function, such as `JSON.stringify`.
  Host,
  /// A value the analysis cannot tell is not a function it cannot see.
  Unknown,
}

struct Walker<'w, 's, 'l, 'a> {
  input: &'w ReachInput<'s, 'l, 'a>,
  info: &'w ReachInfo,
  defs: &'w EffectDefs<'s, 'a>,
  state: &'w mut ReachState,
  module: ModuleIdx,
  functions: Vec<FnId>,
  guards: Vec<Guard>,
  events: Option<&'w mut ReachEvents>,
  /// Set while evaluating a function argument an Effect API calls back: the
  /// function does not escape.
  consumed: bool,
  /// The call whose callbacks are being bound.
  call_span: Option<Span>,
}

impl<'w, 's, 'l, 'a> Walker<'w, 's, 'l, 'a> {
  fn new(
    input: &'w ReachInput<'s, 'l, 'a>,
    info: &'w ReachInfo,
    defs: &'w EffectDefs<'s, 'a>,
    state: &'w mut ReachState,
    module: ModuleIdx,
    events: Option<&'w mut ReachEvents>,
  ) -> Self {
    Walker {
      input,
      info,
      defs,
      state,
      module,
      functions: vec![],
      guards: vec![],
      events,
      consumed: false,
      call_span: None,
    }
  }

  fn shared(&self) -> &'s Shared<'l> {
    self.input.shared
  }

  fn event(&mut self, event: ReachEvent, span: Span) {
    if let Some(events) = self.events.as_deref_mut() {
      events.push((event, self.guards.clone(), self.module, span.start));
    }
  }

  fn observe_all(&mut self, value: Reach, span: Span, why: &'static str) {
    if value.any() {
      self.event(ReachEvent::All(why), span);
    }
  }

  fn symbol_of_ident(&self, ident: &oxc::ast::ast::IdentifierReference<'_>) -> Option<SymbolRef> {
    let reference = ident.reference_id.get()?;
    let symbol = self.shared().scoping(self.module).get_reference(reference).symbol_id()?;
    Some(self.shared().symbols.canonical_ref_for((self.module, symbol).into()))
  }

  fn is_unresolved(&self, ident: &oxc::ast::ast::IdentifierReference<'_>) -> bool {
    ident.reference_id.get().is_some_and(|reference| {
      self.shared().scoping(self.module).get_reference(reference).symbol_id().is_none()
    })
  }

  fn is_global_object(&self, expr: &Expression<'_>) -> bool {
    match strip(expr) {
      Expression::Identifier(ident) => {
        GLOBAL_OBJECTS.contains(&ident.name.as_str()) && self.is_unresolved(ident)
      }
      Expression::StaticMemberExpression(member) => {
        GLOBAL_OBJECTS.contains(&member.property.name.as_str())
          && self.is_global_object(&member.object)
      }
      _ => false,
    }
  }

  /// `Object`, `JSON`, ... possibly through a global object: the builtin's name.
  fn builtin_global(&self, expr: &Expression<'_>) -> Option<String> {
    match strip(expr) {
      Expression::Identifier(ident) => (BUILTIN_GLOBALS.contains(&ident.name.as_str())
        && self.is_unresolved(ident))
      .then(|| ident.name.to_string()),
      Expression::StaticMemberExpression(member) => (BUILTIN_GLOBALS
        .contains(&member.property.name.as_str())
        && self.is_global_object(&member.object))
      .then(|| member.property.name.to_string()),
      _ => None,
    }
  }

  /// A builtin global or one of its members, a global-object member, or a
  /// binding imported from a module outside the bundle.
  fn is_host_ident(&self, ident: &oxc::ast::ast::IdentifierReference<'_>) -> bool {
    match self.symbol_of_ident(ident) {
      Some(symbol) => self.input.externals.contains(&symbol.owner),
      None => {
        ident.reference_id.get().is_some()
          && !GLOBAL_OBJECTS.contains(&ident.name.as_str())
          && !matches!(ident.name.as_str(), "undefined" | "NaN" | "Infinity" | "arguments")
      }
    }
  }

  fn is_host_value(&self, expr: &Expression<'_>) -> bool {
    match strip(expr) {
      Expression::Identifier(ident) => self.is_host_ident(ident),
      Expression::StaticMemberExpression(member) => {
        self.builtin_global(&member.object).is_some() || self.is_host_value(&member.object)
      }
      Expression::ComputedMemberExpression(member) => {
        self.builtin_global(&member.object).is_some() || self.is_host_value(&member.object)
      }
      _ => false,
    }
  }

  fn opaque_function_value(&mut self) {
    if !self.state.opaque_fn {
      self.state.opaque_fn = true;
      self.state.changed = true;
    }
  }

  /// The constant name of a computed member, or whether it is a number.
  fn member_name(&self, expr: &Expression<'_>) -> (Option<String>, bool) {
    match strip(expr) {
      Expression::NumericLiteral(_) | Expression::UpdateExpression(_) => (None, true),
      Expression::Identifier(ident) => {
        if let Some(name) = self.shared().const_string(self.module, expr) {
          return (Some(name), false);
        }
        (None, self.numeric_binding(ident))
      }
      Expression::BinaryExpression(binary)
        if matches!(
          binary.operator,
          BinaryOperator::Subtraction
            | BinaryOperator::Multiplication
            | BinaryOperator::Division
            | BinaryOperator::Remainder
        ) =>
      {
        (None, true)
      }
      _ => (self.shared().const_string(self.module, expr), false),
    }
  }

  /// A `let`/`var` counter: initialised with a number and only written by
  /// `++`, `--` or compound assignments.
  fn numeric_binding(&self, ident: &oxc::ast::ast::IdentifierReference<'_>) -> bool {
    let scoping = self.shared().scoping(self.module);
    let Some(symbol) =
      ident.reference_id.get().and_then(|reference| scoping.get_reference(reference).symbol_id())
    else {
      return false;
    };
    self.info.counters.contains(&(self.module, symbol))
      && scoping
        .get_resolved_references(symbol)
        .all(|reference| !reference.is_write() || reference.is_read())
  }

  fn resolve_function(&self, expr: &Expression<'_>) -> Option<(FnId, bool)> {
    match strip(expr) {
      Expression::FunctionExpression(function) => Some(((self.module, function.span.start), false)),
      Expression::ArrowFunctionExpression(arrow) => Some(((self.module, arrow.span.start), false)),
      Expression::Identifier(ident) => {
        let symbol = self.symbol_of_ident(ident)?;
        self.info.fn_of_symbol.get(&symbol).copied()
      }
      Expression::StaticMemberExpression(_) => {
        match self.shared().resolve_expr(self.module, expr) {
          Some(Resolved::Symbol(symbol)) => self.info.fn_of_symbol.get(&symbol).copied(),
          _ => None,
        }
      }
      _ => None,
    }
  }

  /// The binding a member chain or identifier is rooted at, to record a store.
  fn store_into(&mut self, target: &Expression<'_>, value: Reach) {
    if !value.any() {
      return;
    }
    match strip(target) {
      Expression::Identifier(ident) => match self.symbol_of_ident(ident) {
        Some(symbol) => self.state.join_binding(symbol, value),
        None => {
          let mut changed = self.state.changed;
          ReachState::join_field(&mut self.state.global_value, value.collapse(), &mut changed);
          self.state.changed = changed;
        }
      },
      Expression::StaticMemberExpression(member) => self.store_into(&member.object, value),
      Expression::ComputedMemberExpression(member) => self.store_into(&member.object, value),
      Expression::PrivateFieldExpression(member) => self.store_into(&member.object, value),
      Expression::ThisExpression(_) => {
        let mut changed = self.state.changed;
        ReachState::join_field(&mut self.state.this_value, value, &mut changed);
        self.state.changed = changed;
      }
      _ => {}
    }
  }

  // == expressions

  fn eval(&mut self, expr: &Expression<'a>) -> Reach {
    let value = self.eval_inner(expr);
    if value.any() && self.state.origins.len() < 12 {
      let kind = match expr {
        Expression::CallExpression(_) => "first tainted call",
        Expression::StaticMemberExpression(_) | Expression::ComputedMemberExpression(_) => {
          "first tainted member"
        }
        Expression::Identifier(_) => "first tainted identifier",
        Expression::ThisExpression(_) => "first tainted this",
        _ => "first tainted other",
      };
      self.origin(kind, expr.span());
    }
    value
  }

  fn eval_inner(&mut self, expr: &Expression<'a>) -> Reach {
    match expr {
      Expression::BooleanLiteral(_)
      | Expression::NullLiteral(_)
      | Expression::NumericLiteral(_)
      | Expression::BigIntLiteral(_)
      | Expression::RegExpLiteral(_)
      | Expression::StringLiteral(_) => Reach::NONE,
      Expression::TemplateLiteral(template) => {
        for expression in &template.expressions {
          let value = self.eval(expression);
          self.observe_all(value, expression.span(), "converted to a string");
        }
        Reach::NONE
      }
      Expression::Identifier(ident) => self.eval_identifier(ident),
      Expression::ThisExpression(_) | Expression::Super(_) => self.state.this_value,
      Expression::ArrayExpression(array) => {
        let mut value = Reach::NONE;
        for element in &array.elements {
          value = value.join(match element {
            ArrayExpressionElement::SpreadElement(spread) => self.eval(&spread.argument).element(),
            ArrayExpressionElement::Elision(_) => Reach::NONE,
            _ => self.eval(element.to_expression()),
          });
        }
        value
      }
      Expression::ObjectExpression(object) => self.eval_object(object),
      Expression::FunctionExpression(function) => {
        let id = (self.module, function.span.start);
        if !std::mem::take(&mut self.consumed) {
          self.state.escape(id);
        }
        self.visit_function(function, ScopeFlags::Function);
        Reach::NONE
      }
      Expression::ArrowFunctionExpression(arrow) => {
        let id = (self.module, arrow.span.start);
        if !std::mem::take(&mut self.consumed) {
          self.state.escape(id);
        }
        self.visit_arrow_function_expression(arrow);
        Reach::NONE
      }
      Expression::ClassExpression(class) => {
        self.visit_class(class);
        Reach::NONE
      }
      Expression::AssignmentExpression(assign) => {
        let value = self.eval(&assign.right);
        if assign.operator.is_assign() {
          self.assign_target(&assign.left, value);
        } else {
          // `x += y` converts both sides.
          let current = self.eval_simple_target(&assign.left);
          if !assign.operator.is_logical() {
            self.observe_all(current.join(value), assign.span, "converted by an operator");
          }
          self.assign_target(&assign.left, value);
        }
        value
      }
      Expression::AwaitExpression(await_expr) => self.eval(&await_expr.argument).awaited(),
      Expression::BinaryExpression(binary) if binary.operator == BinaryOperator::Instanceof => {
        self.eval(&binary.left);
        if !self.is_host_value(&binary.right) {
          self.eval(&binary.right);
        }
        Reach::NONE
      }
      Expression::BinaryExpression(binary) => {
        let left = self.eval(&binary.left);
        let right = self.eval(&binary.right);
        match binary.operator {
          BinaryOperator::StrictEquality
          | BinaryOperator::StrictInequality
          | BinaryOperator::Instanceof => {}
          BinaryOperator::In => {
            if right.value() {
              match self.shared().const_string(self.module, &binary.left) {
                Some(name) => self.event(ReachEvent::Name(name), binary.span),
                None => self.event(ReachEvent::All("`in` with a computed name"), binary.span),
              }
            }
          }
          BinaryOperator::Equality | BinaryOperator::Inequality => {
            let null = |e: &Expression<'_>| {
              matches!(strip(e), Expression::NullLiteral(_))
                || matches!(strip(e), Expression::Identifier(i) if i.name == "undefined")
            };
            if !null(&binary.left) && !null(&binary.right) {
              self.observe_all(left.join(right), binary.span, "converted by an operator");
            }
          }
          _ => self.observe_all(left.join(right), binary.span, "converted by an operator"),
        }
        Reach::NONE
      }
      Expression::LogicalExpression(logical) => {
        let left = self.eval(&logical.left);
        let right = self.eval(&logical.right);
        left.join(right)
      }
      Expression::ConditionalExpression(conditional) => {
        self.eval(&conditional.test);
        let consequent = self.eval(&conditional.consequent);
        let alternate = self.eval(&conditional.alternate);
        consequent.join(alternate)
      }
      Expression::SequenceExpression(sequence) => {
        let mut last = Reach::NONE;
        for expression in &sequence.expressions {
          last = self.eval(expression);
        }
        last
      }
      Expression::ParenthesizedExpression(paren) => self.eval(&paren.expression),
      Expression::ChainExpression(chain) => match &chain.expression {
        ChainElement::CallExpression(call) => self.eval_call(call),
        ChainElement::TSNonNullExpression(e) => self.eval(&e.expression),
        ChainElement::StaticMemberExpression(member) => self.eval_static_member(member),
        ChainElement::ComputedMemberExpression(member) => self.eval_computed_member(member),
        ChainElement::PrivateFieldExpression(member) => self.eval(&member.object).member(),
      },
      Expression::CallExpression(call) => self.eval_call(call),
      Expression::NewExpression(new) => self.eval_new(new),
      Expression::TaggedTemplateExpression(tagged) => {
        let mut args = vec![];
        for expression in &tagged.quasi.expressions {
          let value = self.eval(expression);
          args.push(Arg { value, spread: false, functions: vec![], kind: ArgKind::Known });
        }
        self.call_value(&tagged.tag, &args, tagged.span)
      }
      Expression::UnaryExpression(unary) => {
        match unary.operator {
          UnaryOperator::Typeof
          | UnaryOperator::LogicalNot
          | UnaryOperator::Void
          | UnaryOperator::Delete => {
            self.eval(&unary.argument);
          }
          _ => {
            let value = self.eval(&unary.argument);
            self.observe_all(value, unary.span, "converted by an operator");
          }
        }
        Reach::NONE
      }
      Expression::UpdateExpression(update) => {
        let value = self.eval_update_target(&update.argument);
        self.observe_all(value, update.span, "converted by an operator");
        Reach::NONE
      }
      Expression::YieldExpression(yield_expr) => {
        let value =
          yield_expr.argument.as_ref().map_or(Reach::NONE, |argument| self.eval(argument));
        // A tracked key: the pass follows the service per key.
        let tracked = yield_expr
          .argument
          .as_ref()
          .is_some_and(|argument| self.shared().key_of(self.module, argument).is_some());
        let effect_generator = self
          .functions
          .last()
          .is_some_and(|function| self.info.effect_generators.contains(function));
        if yield_expr.delegate && tracked {
          Reach::NONE
        } else if yield_expr.delegate && effect_generator {
          value.success()
        } else if yield_expr.delegate {
          // Delegating to an iterator: its elements, and what its driver sends.
          value.element().join(self.state.esc_params)
        } else {
          if let Some(function) = self.functions.last().copied() {
            self.state.join_ret(function, value);
          }
          self.state.esc_params
        }
      }
      Expression::ImportExpression(import) => {
        self.eval(&import.source);
        Reach::TOP
      }
      Expression::StaticMemberExpression(member) => self.eval_static_member(member),
      Expression::ComputedMemberExpression(member) => self.eval_computed_member(member),
      Expression::PrivateFieldExpression(member) => self.eval(&member.object).member(),
      Expression::PrivateInExpression(private_in) => {
        self.eval(&private_in.right);
        Reach::NONE
      }
      Expression::TSAsExpression(e) => self.eval(&e.expression),
      Expression::TSSatisfiesExpression(e) => self.eval(&e.expression),
      Expression::TSNonNullExpression(e) => self.eval(&e.expression),
      Expression::TSTypeAssertion(e) => self.eval(&e.expression),
      Expression::TSInstantiationExpression(e) => self.eval(&e.expression),
      _ => {
        // JSX and other syntax: children are visited, the value is unknown.
        walk_js::walk_expression(self, expr);
        Reach::TOP
      }
    }
  }

  fn eval_identifier(&mut self, ident: &oxc::ast::ast::IdentifierReference<'a>) -> Reach {
    // A host function used as a value may later be called where the
    // analysis cannot see it. Calls of host functions are handled before.
    if self.is_host_ident(ident) {
      self.opaque_function_value();
    }
    match self.symbol_of_ident(ident) {
      Some(symbol) => {
        if let Some((function, _)) = self.info.fn_of_symbol.get(&symbol).copied() {
          if !std::mem::take(&mut self.consumed) {
            self.state.escape(function);
          }
        }
        self.state.bindings.get(&symbol).copied().unwrap_or_default()
      }
      None => {
        let name = ident.name.as_str();
        if name == "arguments" {
          return self.arguments_value();
        }
        if GLOBAL_OBJECTS.contains(&name) {
          // The global object holds the running fiber.
          return Reach::TOP;
        }
        if BUILTIN_GLOBALS.contains(&name) {
          return Reach::NONE;
        }
        self.state.global_value
      }
    }
  }

  fn arguments_value(&self) -> Reach {
    let Some(function) = self.functions.last() else { return Reach::NONE };
    let mut value = self.state.rest.get(function).copied().unwrap_or_default();
    for param in self.state.params.get(function).into_iter().flatten() {
      value = value.join(*param);
    }
    if self.state.escaped.contains(function) {
      value = value.join(self.state.esc_params);
    }
    value.collapse()
  }

  fn eval_object(&mut self, object: &oxc::ast::ast::ObjectExpression<'a>) -> Reach {
    let site = self.input.site_spans.get(&(self.module, object.span.start)).copied();
    let mut value = Reach::NONE;
    for (index, property) in object.properties.iter().enumerate() {
      match property {
        ObjectPropertyKind::SpreadProperty(spread) => {
          let spread_value = self.eval(&spread.argument);
          if spread_value.value() {
            self.event(ReachEvent::All("spread into an object"), spread.span);
          }
          value = value.join(spread_value.member());
        }
        ObjectPropertyKind::ObjectProperty(property) => {
          if property.computed {
            if let Some(key) = property.key.as_expression() {
              self.eval(key);
            }
          }
          if let Some(site) = site {
            self.guards.push(Guard::Property(site, index));
          }
          let property_value = self.eval(&property.value);
          if let Some(name) = if property.computed { None } else { key_name(&property.key) } {
            self.state.join_prop(&name, property_value);
          }
          if property.kind == PropertyKind::Get {
            if let Expression::FunctionExpression(function) = strip(&property.value) {
              value = value.join(self.state.ret((self.module, function.span.start)));
            }
          } else {
            value = value.join(property_value);
          }
          if site.is_some() {
            self.guards.pop();
          }
        }
      }
    }
    value
  }

  fn eval_static_member(&mut self, member: &oxc::ast::ast::StaticMemberExpression<'a>) -> Reach {
    let name = member.property.name.as_str();
    if self.is_global_object(&member.object) {
      // The global object holds the running fiber under a non-builtin name.
      if BUILTIN_GLOBALS.contains(&name) {
        self.opaque_function_value();
        return Reach::NONE;
      }
      return Reach::TOP;
    }
    if self.builtin_global(&member.object).is_some() || self.is_host_value(&member.object) {
      // `JSON.stringify` or a member of an external module, used as a value.
      self.opaque_function_value();
      return Reach::NONE;
    }
    if matches!(
      self.shared().resolve_expr(self.module, &member.object),
      Some(Resolved::Namespace(_))
    ) {
      return self.eval_namespace_member(member);
    }
    let object = self.eval(&member.object);
    self.read(object, Some(name), false, member.span)
  }

  fn eval_computed_member(
    &mut self,
    member: &oxc::ast::ast::ComputedMemberExpression<'a>,
  ) -> Reach {
    if !self.is_global_object(&member.object)
      && (self.builtin_global(&member.object).is_some() || self.is_host_value(&member.object))
    {
      self.eval(&member.expression);
      self.opaque_function_value();
      return Reach::NONE;
    }
    let object = self.eval(&member.object);
    self.eval(&member.expression);
    if self.is_global_object(&member.object) {
      return match self.shared().const_string(self.module, &member.expression) {
        Some(name) if BUILTIN_GLOBALS.contains(&name.as_str()) => Reach::NONE,
        _ => Reach::TOP,
      };
    }
    let (name, numeric) = self.member_name(&member.expression);
    self.read(object, name.as_deref(), numeric, member.span)
  }

  /// `ns.binding` through module namespaces: the binding's value.
  fn eval_namespace_member(&mut self, member: &oxc::ast::ast::StaticMemberExpression<'a>) -> Reach {
    let Some(Resolved::Namespace(target)) = self.shared().resolve_expr(self.module, &member.object)
    else {
      return Reach::NONE;
    };
    match self.shared().resolve_export(target, member.property.name.as_str()) {
      Some(Resolved::Symbol(symbol)) => {
        if let Some((function, _)) = self.info.fn_of_symbol.get(&symbol).copied() {
          if !std::mem::take(&mut self.consumed) {
            self.state.escape(function);
          }
        }
        self.state.bindings.get(&symbol).copied().unwrap_or_default()
      }
      // A namespace object used as a value is a separate global bailout.
      Some(Resolved::Namespace(_)) | None => Reach::NONE,
    }
  }

  /// Reading a property of `object` by `name` (or an unknown or numeric name).
  fn read(&mut self, object: Reach, name: Option<&str>, numeric: bool, span: Span) -> Reach {
    let getters = name.map_or(Reach::NONE, |name| self.getter_returns(name));
    if !object.value() {
      return object.join(getters);
    }
    match (name, numeric) {
      (Some(name), _) => self.event(ReachEvent::Name(name.to_string()), span),
      (None, true) => self.event(ReachEvent::Numeric, span),
      (None, false) => self.event(ReachEvent::All("read by a computed name"), span),
    }
    match name {
      Some(name) if PRIMITIVE_MEMBERS.contains(&name) => {
        self.state.props.get(name).copied().unwrap_or_default().join(getters)
      }
      None if numeric => object.element(),
      _ => Reach::TOP,
    }
  }

  fn getter_returns(&self, name: &str) -> Reach {
    let mut value = Reach::NONE;
    for getter in self.info.getters.get(name).into_iter().flatten() {
      value = value.join(self.state.ret(*getter));
    }
    value
  }

  /// The current value of an assignment target, for compound assignments.
  fn eval_simple_target(&mut self, target: &AssignmentTarget<'a>) -> Reach {
    match target {
      AssignmentTarget::AssignmentTargetIdentifier(ident) => self.eval_identifier(ident),
      AssignmentTarget::StaticMemberExpression(member) => self.eval_static_member(member),
      AssignmentTarget::ComputedMemberExpression(member) => self.eval_computed_member(member),
      AssignmentTarget::PrivateFieldExpression(member) => self.eval(&member.object).member(),
      _ => Reach::TOP,
    }
  }

  /// The current value of a `++`/`--` operand.
  fn eval_update_target(&mut self, target: &SimpleAssignmentTarget<'a>) -> Reach {
    match target {
      SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => self.eval_identifier(ident),
      SimpleAssignmentTarget::StaticMemberExpression(member) => self.eval_static_member(member),
      SimpleAssignmentTarget::ComputedMemberExpression(member) => self.eval_computed_member(member),
      SimpleAssignmentTarget::PrivateFieldExpression(member) => self.eval(&member.object).member(),
      SimpleAssignmentTarget::TSAsExpression(e) => self.eval(&e.expression),
      SimpleAssignmentTarget::TSSatisfiesExpression(e) => self.eval(&e.expression),
      SimpleAssignmentTarget::TSNonNullExpression(e) => self.eval(&e.expression),
      SimpleAssignmentTarget::TSTypeAssertion(e) => self.eval(&e.expression),
    }
  }

  fn assign_target(&mut self, target: &AssignmentTarget<'a>, value: Reach) {
    match target {
      AssignmentTarget::AssignmentTargetIdentifier(ident) => match self.symbol_of_ident(ident) {
        Some(symbol) => self.state.join_binding(symbol, value),
        None => {
          let mut changed = self.state.changed;
          ReachState::join_field(&mut self.state.global_value, value.collapse(), &mut changed);
          self.state.changed = changed;
        }
      },
      AssignmentTarget::StaticMemberExpression(member) => {
        let object = self.eval(&member.object);
        let name = member.property.name.as_str();
        self.state.join_prop(name, value);
        if object.value() {
          // Writing a member of a runtime object, possibly a service.
          self.event(ReachEvent::Name(name.to_string()), member.span);
        }
        for setter in self.info.setters.get(name).cloned().unwrap_or_default() {
          self.state.join_param(setter, 0, value);
        }
        if self.is_global_object(&member.object) {
          let mut changed = self.state.changed;
          ReachState::join_field(&mut self.state.global_value, value.collapse(), &mut changed);
          self.state.changed = changed;
        } else {
          self.store_into(&member.object, value);
        }
      }
      AssignmentTarget::ComputedMemberExpression(member) => {
        let object = self.eval(&member.object);
        self.eval(&member.expression);
        let (name, numeric) = self.member_name(&member.expression);
        if object.value() {
          match (name, numeric) {
            (Some(name), _) => self.event(ReachEvent::Name(name), member.span),
            (None, true) => self.event(ReachEvent::Numeric, member.span),
            (None, false) => self.event(ReachEvent::All("written by a computed name"), member.span),
          }
        }
        if self.is_global_object(&member.object) {
          let mut changed = self.state.changed;
          ReachState::join_field(&mut self.state.global_value, value.collapse(), &mut changed);
          self.state.changed = changed;
        } else {
          self.store_into(&member.object, value);
        }
      }
      AssignmentTarget::PrivateFieldExpression(member) => {
        self.eval(&member.object);
        self.store_into(&member.object, value);
      }
      AssignmentTarget::ArrayAssignmentTarget(array) => {
        for element in array.elements.iter().flatten() {
          self.assign_maybe_default(element, value.element());
        }
        if let Some(rest) = &array.rest {
          self.assign_target(&rest.target, value.element());
        }
      }
      AssignmentTarget::ObjectAssignmentTarget(object) => {
        for property in &object.properties {
          match property {
            AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(property) => {
              if value.value() {
                self.event(ReachEvent::Name(property.binding.name.to_string()), property.span);
              }
              let mut item = value.member();
              if let Some(init) = &property.init {
                item = item.join(self.eval(init));
              }
              if let Some(symbol) = self.symbol_of_ident(&property.binding) {
                self.state.join_binding(symbol, item);
              }
            }
            AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
              self.destructure_key(&property.name, property.computed, value, property.span);
              self.assign_maybe_default(&property.binding, value.member());
            }
          }
        }
        if let Some(rest) = &object.rest {
          if value.value() {
            self.event(ReachEvent::All("rest of a destructuring"), rest.span);
          }
          self.assign_target(&rest.target, value.member());
        }
      }
      AssignmentTarget::TSAsExpression(e) => self.assign_expression_target(&e.expression, value),
      AssignmentTarget::TSSatisfiesExpression(e) => {
        self.assign_expression_target(&e.expression, value);
      }
      AssignmentTarget::TSNonNullExpression(e) => {
        self.assign_expression_target(&e.expression, value);
      }
      AssignmentTarget::TSTypeAssertion(e) => self.assign_expression_target(&e.expression, value),
    }
  }

  fn assign_expression_target(&mut self, expr: &Expression<'a>, value: Reach) {
    self.eval(expr);
    self.store_into(expr, value);
  }

  fn assign_maybe_default(&mut self, target: &AssignmentTargetMaybeDefault<'a>, value: Reach) {
    match target {
      AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(with_default) => {
        let default = self.eval(&with_default.init);
        self.assign_target(&with_default.binding, value.join(default));
      }
      _ => {
        if let Some(target) = target.as_assignment_target() {
          self.assign_target(target, value);
        }
      }
    }
  }

  /// A destructured key read from `value`.
  fn destructure_key(&mut self, key: &PropertyKey<'a>, computed: bool, value: Reach, span: Span) {
    let name = if computed {
      let expr = key.as_expression();
      if let Some(expr) = expr {
        self.eval(expr);
      }
      expr.and_then(|expr| self.shared().const_string(self.module, expr))
    } else {
      key_name(key)
    };
    if value.value() {
      match name {
        Some(name) => self.event(ReachEvent::Name(name), span),
        None => self.event(ReachEvent::All("destructured by a computed name"), span),
      }
    }
  }

  fn bind_pattern(&mut self, pattern: &BindingPattern<'a>, value: Reach) {
    match pattern {
      BindingPattern::BindingIdentifier(id) => {
        if let Some(symbol) = id.symbol_id.get() {
          let symbol = self.shared().symbols.canonical_ref_for((self.module, symbol).into());
          self.state.join_binding(symbol, value);
        }
      }
      BindingPattern::ObjectPattern(object) => {
        for property in &object.properties {
          self.destructure_key(&property.key, property.computed, value, property.span);
          self.bind_pattern(&property.value, value.member());
        }
        if let Some(rest) = &object.rest {
          if value.value() {
            self.event(ReachEvent::All("rest of a destructuring"), rest.span);
          }
          self.bind_pattern(&rest.argument, value.member());
        }
      }
      BindingPattern::ArrayPattern(array) => {
        for element in array.elements.iter().flatten() {
          self.bind_pattern(element, value.element());
        }
        if let Some(rest) = &array.rest {
          self.bind_pattern(&rest.argument, value.element());
        }
      }
      BindingPattern::AssignmentPattern(assignment) => {
        let default = self.eval(&assignment.right);
        self.bind_pattern(&assignment.left, value.join(default));
      }
    }
  }

  fn bind_params(&mut self, function: FnId, params: &FormalParameters<'a>) {
    let escaped = self.state.escaped.contains(&function);
    let extra = if escaped { self.state.esc_params } else { Reach::NONE };
    let values = self.state.params.get(&function).cloned().unwrap_or_default();
    let rest = self.state.rest.get(&function).copied().unwrap_or_default();
    for (index, param) in params.items.iter().enumerate() {
      let mut value = values.get(index).copied().unwrap_or_default().join(rest).join(extra);
      if let Some(initializer) = &param.initializer {
        value = value.join(self.eval(initializer));
      }
      self.bind_pattern(&param.pattern, value);
    }
    if let Some(rest_param) = &params.rest {
      let mut value = rest.join(extra);
      for spare in values.iter().skip(params.items.len()) {
        value = value.join(*spare);
      }
      self.bind_pattern(&rest_param.rest.argument, value);
    }
  }

  // == calls

  fn eval_args(&mut self, arguments: &ArenaVec<'a, Argument<'a>>, consume: bool) -> Vec<Arg> {
    let mut out = vec![];
    for argument in arguments {
      match argument {
        Argument::SpreadElement(spread) => {
          let value = self.eval(&spread.argument).element();
          out.push(Arg { value, spread: true, functions: vec![], kind: ArgKind::Unknown });
        }
        _ => {
          let expr = argument.to_expression();
          let functions = self.functions_in(expr);
          let kind = self.arg_kind(expr, &functions);
          self.consumed = consume && !functions.is_empty() && self.is_direct_function(expr);
          let value = self.eval(expr);
          self.consumed = false;
          out.push(Arg { value, spread: false, functions, kind });
        }
      }
    }
    out
  }

  /// What calling the argument back would run.
  fn arg_kind(&self, expr: &Expression<'a>, functions: &[FnId]) -> ArgKind {
    let expr = strip(expr);
    if !functions.is_empty() && self.is_direct_function(expr) {
      return ArgKind::Known;
    }
    let global_member = match expr {
      Expression::StaticMemberExpression(member) => self.is_global_object(&member.object),
      Expression::ComputedMemberExpression(member) => self.is_global_object(&member.object),
      _ => false,
    };
    if global_member || self.is_host_value(expr) {
      return ArgKind::Host;
    }
    match expr {
      Expression::CallExpression(call) => {
        if let Some(api) = self.shared().api_of(self.module, &call.callee) {
          if self.tracked_lookup(api, &call.arguments, 1) {
            return ArgKind::Lookup;
          }
          return ArgKind::Effect;
        }
        let mut callee = strip(&call.callee);
        while let Expression::CallExpression(inner) = callee {
          callee = strip(&inner.callee);
        }
        if self.shared().api_of(self.module, callee).is_some() {
          return ArgKind::Effect;
        }
        ArgKind::Unknown
      }
      Expression::ObjectExpression(object) => {
        let unknown = object.properties.iter().any(|property| match property {
          ObjectPropertyKind::ObjectProperty(property) => {
            property.kind == PropertyKind::Init
              && self.resolve_function(&property.value).is_none()
              && self.arg_kind(&property.value, &[]) == ArgKind::Unknown
          }
          ObjectPropertyKind::SpreadProperty(_) => true,
        });
        if unknown { ArgKind::Unknown } else { ArgKind::Known }
      }
      Expression::ArrayExpression(array) => {
        let unknown = array.elements.iter().any(|element| match element {
          ArrayExpressionElement::SpreadElement(_) => true,
          ArrayExpressionElement::Elision(_) => false,
          element => {
            let element = element.to_expression();
            self.resolve_function(element).is_none()
              && self.arg_kind(element, &[]) == ArgKind::Unknown
          }
        });
        if unknown { ArgKind::Unknown } else { ArgKind::Known }
      }
      Expression::StringLiteral(_)
      | Expression::NumericLiteral(_)
      | Expression::BooleanLiteral(_)
      | Expression::NullLiteral(_)
      | Expression::BigIntLiteral(_)
      | Expression::RegExpLiteral(_)
      | Expression::TemplateLiteral(_)
      | Expression::BinaryExpression(_)
      | Expression::UnaryExpression(_)
      | Expression::ClassExpression(_) => ArgKind::Known,
      Expression::Identifier(ident) => match self.symbol_of_ident(ident) {
        Some(symbol) if self.info.fn_of_symbol.contains_key(&symbol) => ArgKind::Known,
        Some(symbol) if self.shared().keys.contains_key(&symbol) => ArgKind::Effect,
        Some(symbol) => match self.shared().api_of(self.module, expr) {
          Some(_) => ArgKind::Effect,
          None => self.info.const_kinds.get(&symbol).copied().unwrap_or(ArgKind::Unknown),
        },
        None if ident.name == "undefined" => ArgKind::Known,
        None => ArgKind::Unknown,
      },
      Expression::StaticMemberExpression(_) => {
        match self.shared().resolve_expr(self.module, expr) {
          Some(Resolved::Symbol(symbol)) => {
            if self.info.fn_of_symbol.contains_key(&symbol) {
              ArgKind::Known
            } else if self.shared().api_of(self.module, expr).is_some()
              || self.shared().keys.contains_key(&symbol)
            {
              ArgKind::Effect
            } else {
              self.info.const_kinds.get(&symbol).copied().unwrap_or(ArgKind::Unknown)
            }
          }
          _ => ArgKind::Unknown,
        }
      }
      _ => ArgKind::Unknown,
    }
  }

  fn is_direct_function(&self, expr: &Expression<'_>) -> bool {
    matches!(
      strip(expr),
      Expression::FunctionExpression(_)
        | Expression::ArrowFunctionExpression(_)
        | Expression::Identifier(_)
    )
  }

  /// Functions passed as an argument: directly, or as values of an object or
  /// array literal.
  fn functions_in(&self, expr: &Expression<'_>) -> Vec<FnId> {
    let mut out = vec![];
    match strip(expr) {
      Expression::ObjectExpression(object) => {
        for property in &object.properties {
          if let ObjectPropertyKind::ObjectProperty(property) = property {
            if let Some((function, _)) = self.resolve_function(&property.value) {
              out.push(function);
            }
          }
        }
      }
      Expression::ArrayExpression(array) => {
        for element in &array.elements {
          if let Some(expr) = element.as_expression() {
            if let Some((function, _)) = self.resolve_function(expr) {
              out.push(function);
            }
          }
        }
      }
      other => {
        if let Some((function, _)) = self.resolve_function(other) {
          out.push(function);
        }
      }
    }
    out
  }

  fn join_all(args: &[Arg]) -> Reach {
    args.iter().fold(Reach::NONE, |acc, arg| acc.join(arg.value))
  }

  /// Calls a known function with these arguments: its result.
  fn call_function(&mut self, function: FnId, effect_fn: bool, args: &[Arg], this: Reach) -> Reach {
    let mut index = 0;
    for arg in args {
      if arg.spread {
        self.state.join_rest(function, arg.value.collapse());
        for i in index..index + 8 {
          self.state.join_param(function, i, arg.value.collapse());
        }
      } else {
        self.state.join_param(function, index, arg.value);
        index += 1;
      }
    }
    let mut changed = self.state.changed;
    ReachState::join_field(&mut self.state.this_value, this, &mut changed);
    self.state.changed = changed;
    let ret = self.state.ret(function);
    if effect_fn || self.info.async_fns.contains(&function) {
      ret.effect_of()
    } else if self.info.generator_fns.contains(&function) {
      ret.collapse()
    } else {
      ret
    }
  }

  /// Calls back each function passed in `args` with `param` for every parameter.
  fn call_back(&mut self, args: &[Arg], param: Reach) -> Reach {
    let mut ret = Reach::NONE;
    for arg in args {
      for function in &arg.functions {
        for index in 0..4 {
          self.state.join_param(*function, index, param);
        }
        self.state.join_rest(*function, param);
        ret = ret.join(self.state.ret(*function));
      }
      match arg.kind {
        ArgKind::Known | ArgKind::Lookup => {}
        ArgKind::Effect => ret = ret.join(param.join(arg.value).collapse()),
        ArgKind::Host | ArgKind::Unknown => {
          if param.any() {
            // An escaped function of the program sees what it is called with;
            // a builtin or external one may read every property.
            if arg.kind == ArgKind::Host || self.state.opaque_fn {
              if let Some(span) = self.call_span {
                self.event(ReachEvent::All("passed to a callback the analysis cannot see"), span);
              }
            }
            let mut changed = self.state.changed;
            ReachState::join_field(&mut self.state.esc_params, param.collapse(), &mut changed);
            self.state.changed = changed;
          }
          ret = ret.join(self.state.esc_ret).join(arg.value.collapse());
        }
      }
    }
    ret
  }

  fn eval_call(&mut self, call: &CallExpression<'a>) -> Reach {
    let outer = self.call_span.replace(call.span);
    let value = self.eval_call_inner(call);
    self.call_span = outer;
    value
  }

  fn eval_call_inner(&mut self, call: &CallExpression<'a>) -> Reach {
    let callee = strip(&call.callee);
    // `x.pipe(f, g, ...)` and `pipe(x, f, g, ...)`.
    if let Expression::StaticMemberExpression(member) = callee {
      if member.property.name == "pipe"
        && !matches!(
          self.shared().resolve_expr(self.module, &member.object),
          Some(Resolved::Namespace(_))
        )
      {
        let value = self.eval(&member.object);
        return self.eval_pipe(value, &call.arguments);
      }
    }
    if let Some(api) = self.shared().api_of(self.module, callee) {
      if matches!(api, "Function.pipe" | "pipe") && !call.arguments.is_empty() {
        let first = match &call.arguments[0] {
          Argument::SpreadElement(spread) => self.eval(&spread.argument).collapse(),
          argument => self.eval(argument.to_expression()),
        };
        let rest = &call.arguments;
        return self.eval_pipe_from(first, rest, 1);
      }
      let api = api.to_string();
      self.eval_member_object(callee);
      let args = self.eval_args(&call.arguments, true);
      if self.shared().key_creations.contains(&(self.module, call.callee.span().start))
        || self.tracked_lookup(&api, &call.arguments, 0)
      {
        // A declared key, or a lookup by tracked keys: the pass follows those
        // services per key.
        return Reach::NONE;
      }
      if let Some(Resolved::Symbol(symbol)) = self.shared().resolve_expr(self.module, callee) {
        if let Some(result) = self.derive_api(symbol, &args) {
          self.state.derived.insert(api);
          return result;
        }
      }
      self.state.underived.insert(api.clone());
      return self.apply_api(&api, None, args, call.span);
    }
    // A known function, called directly or through `.call`.
    if let Some((function, effect_fn)) = self.resolve_function(callee) {
      if let Expression::FunctionExpression(f) = callee {
        let id = (self.module, f.span.start);
        self.consumed = true;
        self.visit_function(f, ScopeFlags::Function);
        self.consumed = false;
        let _ = id;
      } else if let Expression::ArrowFunctionExpression(arrow) = callee {
        self.visit_arrow_function_expression(arrow);
      } else {
        self.consumed = true;
        self.eval(callee);
        self.consumed = false;
      }
      let args = self.eval_args(&call.arguments, false);
      return self.call_function(function, effect_fn, &args, Reach::NONE);
    }
    match callee {
      Expression::StaticMemberExpression(member) => {
        let name = member.property.name.to_string();
        if let Some(builtin) = self.builtin_global(&member.object) {
          let args = self.eval_args(&call.arguments, false);
          return self.call_builtin(&format!("{builtin}.{name}"), &args, call.span);
        }
        if self.is_global_object(&member.object) || self.is_host_value(&member.object) {
          let args = self.eval_args(&call.arguments, false);
          return self.call_unknown_in(Reach::TOP, &args, call.span, true);
        }
        // `f.call(thisArg, ...args)` and `f.apply(thisArg, args)`.
        if matches!(name.as_str(), "call" | "apply") {
          if let Some((function, effect_fn)) = self.resolve_function(&member.object) {
            self.consumed = true;
            self.eval(&member.object);
            self.consumed = false;
            let mut args = self.eval_args(&call.arguments, false);
            let this = if args.is_empty() { Reach::NONE } else { args.remove(0).value };
            if name == "apply" {
              args = args.into_iter().map(|arg| Arg { spread: true, ..arg }).collect();
            }
            return self.call_function(function, effect_fn, &args, this);
          }
        }
        let receiver = self.eval(&member.object);
        let args = self.eval_args(&call.arguments, false);
        self.call_method(receiver, Some(&name), false, &args, Some(&member.object), call.span)
      }
      Expression::ComputedMemberExpression(member) => {
        let receiver = self.eval(&member.object);
        self.eval(&member.expression);
        let (name, numeric) = self.member_name(&member.expression);
        let args = self.eval_args(&call.arguments, false);
        if self.is_global_object(&member.object) || self.is_host_value(&member.object) {
          return self.call_unknown_in(Reach::TOP, &args, call.span, true);
        }
        self.call_method(receiver, name.as_deref(), numeric, &args, Some(&member.object), call.span)
      }
      Expression::Identifier(ident) if self.is_unresolved(ident) => {
        let name = ident.name.to_string();
        let args = self.eval_args(&call.arguments, false);
        if BUILTIN_GLOBALS.contains(&name.as_str()) {
          return self.call_builtin(&name, &args, call.span);
        }
        let callee_value = self.state.global_value;
        self.call_unknown_in(callee_value, &args, call.span, true)
      }
      Expression::Identifier(ident) if self.is_host_ident(ident) => {
        let args = self.eval_args(&call.arguments, false);
        self.call_unknown_in(Reach::TOP, &args, call.span, true)
      }
      // `Api(options)(value)`: a data-last call applied directly.
      Expression::CallExpression(inner)
        if self.shared().api_of(self.module, &inner.callee).is_some() =>
      {
        let api = self.shared().api_of(self.module, &inner.callee).unwrap_or_default().to_string();
        self.eval_member_object(strip(&inner.callee));
        let inner_args = self.eval_args(&inner.arguments, true);
        let args = self.eval_args(&call.arguments, false);
        let piped = Self::join_all(&args);
        if self.tracked_lookup(&api, &inner.arguments, 1)
          || self.shared().key_creations.contains(&(self.module, call.callee.span().start))
        {
          return Reach::NONE;
        }
        self.apply_api_or_derive(&inner.callee, &api, piped, inner_args, call.span)
      }
      _ => {
        let args = self.eval_args(&call.arguments, false);
        self.call_value(callee, &args, call.span)
      }
    }
  }

  /// Evaluates the object of a callee member for its side effects.
  fn eval_member_object(&mut self, callee: &Expression<'a>) {
    match callee {
      Expression::StaticMemberExpression(member) => {
        if !matches!(
          self.shared().resolve_expr(self.module, &member.object),
          Some(Resolved::Namespace(_))
        ) {
          self.eval(&member.object);
        }
      }
      Expression::ComputedMemberExpression(member) => {
        self.eval(&member.object);
        self.eval(&member.expression);
      }
      _ => {}
    }
  }

  /// A keyed lookup whose keys are all tracked; `piped` counts arguments
  /// supplied by `pipe` or a curried application.
  fn tracked_lookup(
    &self,
    api: &str,
    arguments: &ArenaVec<'a, Argument<'a>>,
    piped: usize,
  ) -> bool {
    if !super::KEYED_LOOKUPS.contains(&api) && api != "Context.getOrUndefinedUnsafe" {
      return false;
    }
    if arguments.iter().any(|argument| matches!(argument, Argument::SpreadElement(_))) {
      return false;
    }
    let Some(positions) = super::key_arguments(api, arguments.len() + piped) else { return false };
    positions.iter().all(|position| {
      position
        .checked_sub(piped)
        .and_then(|index| arguments.get(index))
        .and_then(Argument::as_expression)
        .is_some_and(|argument| self.shared().key_of(self.module, argument).is_some())
    })
  }

  fn call_value(&mut self, callee: &Expression<'a>, args: &[Arg], span: Span) -> Reach {
    let callee_value = self.eval(callee);
    self.call_unknown(callee_value, args, span)
  }

  fn origin(&mut self, what: &'static str, span: Span) {
    if !self.state.origins.iter().any(|(w, _, _)| *w == what) {
      self.state.origins.push((what, self.module, span.start));
    }
  }

  /// A call into code the analysis cannot see.
  fn call_unknown(&mut self, callee: Reach, args: &[Arg], span: Span) -> Reach {
    self.call_unknown_in(callee, args, span, false)
  }

  /// A call of a function the analysis cannot identify. An escaped function
  /// of the program sees its arguments through `esc_params`; a `host`
  /// function, or any callee once a host function flowed as a value, may read
  /// every property of them.
  fn call_unknown_in(&mut self, callee: Reach, args: &[Arg], span: Span, host: bool) -> Reach {
    let all = Self::join_all(args);
    if all.any() {
      self.origin("esc_params (unknown call)", span);
    }
    if host || self.state.opaque_fn || callee.value() {
      self.observe_all(all, span, "passed to a function the analysis cannot see");
    }
    let mut changed = self.state.changed;
    ReachState::join_field(&mut self.state.esc_params, all.collapse(), &mut changed);
    self.state.changed = changed;
    // Functions passed along may be called with what the call gave it, or,
    // for a callee that is a runtime object, anything.
    let param = if callee.value() { Reach::TOP } else { all.collapse() };
    let callbacks: Vec<Arg> = args
      .iter()
      .filter(|arg| !arg.functions.is_empty())
      .map(|arg| Arg {
        value: arg.value,
        spread: arg.spread,
        functions: arg.functions.clone(),
        kind: ArgKind::Known,
      })
      .collect();
    self.call_back(&callbacks, param);
    let mut result = self.state.esc_ret;
    if callee.any() {
      result = Reach::TOP;
    }
    result
  }

  fn call_builtin(&mut self, name: &str, args: &[Arg], span: Span) -> Reach {
    let all = Self::join_all(args);
    if TRANSFER_STATICS.contains(&name) {
      let callback_param = all.element();
      let ret = self.call_back(args, callback_param);
      return match name {
        "Promise.all" | "Promise.allSettled" | "Promise.race" | "Promise.any" => {
          all.element().awaited().effect_of()
        }
        "Promise.resolve" => all.awaited().effect_of(),
        "Object.fromEntries" | "Array.from" | "Array.of" => all.element().join(ret),
        "Object.freeze"
        | "Object.seal"
        | "Object.preventExtensions"
        | "Object.create"
        | "Object.getPrototypeOf"
        | "Reflect.getPrototypeOf" => all,
        _ => Reach::NONE,
      };
    }
    if name == "JSON.parse" {
      self.observe_all(all, span, "converted to a string");
      self.call_back(args, Reach::NONE);
      return Reach::NONE;
    }
    // Every other builtin may enumerate, format or convert its arguments.
    self.observe_all(all, span, "passed to a builtin that reads every property");
    self.call_back(args, all.collapse());
    match name {
      "Object.assign" => all,
      "Object.values" | "Object.entries" | "Reflect.get" => all.member(),
      _ => Reach::NONE,
    }
  }

  /// `receiver.name(args)`.
  fn call_method(
    &mut self,
    receiver: Reach,
    name: Option<&str>,
    numeric: bool,
    args: &[Arg],
    object: Option<&Expression<'a>>,
    span: Span,
  ) -> Reach {
    let all = Self::join_all(args);
    let methods = name.and_then(|name| self.info.methods.get(name)).cloned().unwrap_or_default();
    let mut ret = Reach::NONE;
    for (function, effect_fn) in &methods {
      ret = ret.join(self.call_function(*function, *effect_fn, args, receiver));
    }
    let builtin = name
      .is_none_or(|name| BUILTIN_MEMBERS.contains(&name) || self.info.effect_names.contains(name));
    if receiver.value() {
      match (name, numeric) {
        (Some(name), _) => {
          self.event(ReachEvent::Name(name.to_string()), span);
          if RECEIVER_REFLECTIVE_METHODS.contains(&name) {
            self.event(ReachEvent::All("runtime object converted or inspected"), span);
          }
        }
        (None, true) => self.event(ReachEvent::Numeric, span),
        (None, false) => self.event(ReachEvent::All("method called by a computed name"), span),
      }
      self.call_back(args, Reach::TOP);
      if let Some(name) = name.filter(|name| RUNNER_METHODS.contains(name)) {
        return ret.join(self.runner(name, all));
      }
      return if builtin { Reach::TOP } else { ret };
    }
    let touched = receiver.join(all);
    if touched.any() {
      if name.is_none_or(|name| COERCING_METHODS.contains(&name)) {
        self.event(ReachEvent::All("converted by a builtin method"), span);
      }
      if name.is_none_or(|name| !NONCALLING_METHODS.contains(&name)) {
        if all.any() {
          self.origin("esc_params (method)", span);
        }
        let mut changed = self.state.changed;
        ReachState::join_field(&mut self.state.esc_params, all.collapse(), &mut changed);
        self.state.changed = changed;
      }
      if all.any() {
        if let Some(object) = object {
          self.store_into(object, all.collapse());
        }
      }
    }
    let callback_ret = self.call_back(args, receiver.element().join(all.collapse()));
    if let Some(name) = name.filter(|name| RUNNER_METHODS.contains(name)) {
      return ret.join(self.runner(name, all));
    }
    if builtin {
      ret = ret.join(touched.collapse()).join(callback_ret);
      if name.is_none_or(|name| !NONCALLING_METHODS.contains(&name)) {
        ret = ret.join(self.state.esc_ret);
      }
    }
    if name == Some("then") || name == Some("catch") || name == Some("finally") {
      ret = ret.join(receiver).join(callback_ret.awaited().effect_of());
    }
    ret
  }

  fn runner(&mut self, name: &str, effect: Reach) -> Reach {
    match name {
      "runPromise" | "runPromiseExit" => Reach(effect.0 & 0b110),
      "runSync" | "runSyncExit" => {
        self.async_fiber_errors();
        effect.success()
      }
      "runFork" => effect.fiber_of(),
      _ => Reach::TOP,
    }
  }

  /// `runSync` of an asynchronous Effect throws an error holding the fiber.
  fn async_fiber_errors(&mut self) {
    let mut changed = self.state.changed;
    ReachState::join_field(&mut self.state.thrown, Reach::TOP, &mut changed);
    self.state.changed = changed;
  }

  fn eval_new(&mut self, new: &oxc::ast::ast::NewExpression<'a>) -> Reach {
    let callee = strip(&new.callee);
    if let Some(name) = self.builtin_global(callee) {
      let args = self.eval_args(&new.arguments, false);
      let all = Self::join_all(&args);
      return match name.as_str() {
        "Map" | "Set" | "WeakMap" | "WeakSet" | "WeakRef" | "Array" => {
          self.call_back(&args, all.element());
          all.element()
        }
        "Promise" => {
          // The executor's `resolve` stores what it is given.
          let ret = self.call_back(&args, Reach::NONE);
          ret.effect_of().join(self.state.esc_params.effect_of())
        }
        "Proxy" => {
          // Handler traps receive the target.
          self.call_back(&args, all.collapse());
          all
        }
        _ => {
          self.observe_all(all, new.span, "passed to a builtin that reads every property");
          Reach::NONE
        }
      };
    }
    if let Expression::Identifier(ident) = callee {
      if let Some(symbol) = self.symbol_of_ident(ident) {
        if self.info.classes.contains_key(&symbol) {
          let args = self.eval_args(&new.arguments, false);
          let all = Self::join_all(&args);
          // Every constructor up the chain receives the arguments; a class
          // the analysis cannot see may do anything with them.
          let mut current = Some(symbol);
          let mut depth = 0;
          while let Some(class) = current.take() {
            depth += 1;
            let Some((constructor, heritage)) = self.info.classes.get(&class).copied() else {
              self.observe_all(all, new.span, "passed to an unknown constructor");
              break;
            };
            if let Some(constructor) = constructor {
              self.call_function(constructor, false, &args, Reach::NONE);
            }
            match heritage {
              Heritage::None | Heritage::Effect => {}
              Heritage::Class(parent) if depth < 32 => current = Some(parent),
              Heritage::Class(_) | Heritage::Unknown => {
                self.observe_all(all, new.span, "passed to an unknown constructor");
              }
            }
          }
          self.call_back(&args, all.collapse());
          return self.state.this_value.join(all.collapse());
        }
      }
    }
    let callee_value = self.eval(callee);
    let args = self.eval_args(&new.arguments, false);
    let all = Self::join_all(&args);
    if self.shared().api_of(self.module, callee).is_some() {
      // An Effect class: the instance holds its arguments.
      self.call_back(&args, all.collapse());
      return all.collapse().join(callee_value);
    }
    let result = self.call_unknown(callee_value, &args, new.span);
    result.join(self.state.this_value)
  }

  fn eval_pipe(&mut self, value: Reach, arguments: &ArenaVec<'a, Argument<'a>>) -> Reach {
    self.eval_pipe_from(value, arguments, 0)
  }

  fn eval_pipe_from(
    &mut self,
    mut value: Reach,
    arguments: &ArenaVec<'a, Argument<'a>>,
    start: usize,
  ) -> Reach {
    for argument in arguments.iter().skip(start) {
      let Some(expr) = argument.as_expression() else {
        let spread = match argument {
          Argument::SpreadElement(spread) => self.eval(&spread.argument),
          _ => Reach::TOP,
        };
        value = self.call_unknown(
          spread,
          &[Arg { value, spread: false, functions: vec![], kind: ArgKind::Known }],
          argument.span(),
        );
        continue;
      };
      match strip(expr) {
        Expression::CallExpression(call) => {
          if let Some(api) = self.shared().api_of(self.module, &call.callee).map(str::to_string) {
            self.eval_member_object(strip(&call.callee));
            let args = self.eval_args(&call.arguments, true);
            value = if self.tracked_lookup(&api, &call.arguments, 1) {
              Reach::NONE
            } else {
              self.apply_api_or_derive(&call.callee, &api, value, args, call.span)
            };
            continue;
          }
          let applied = self.eval(expr);
          value = self.call_unknown(
            applied,
            &[Arg { value, spread: false, functions: vec![], kind: ArgKind::Known }],
            call.span,
          );
        }
        other => {
          if let Some((function, effect_fn)) = self.resolve_function(other) {
            self.consumed = true;
            self.eval(other);
            self.consumed = false;
            value = self.call_function(
              function,
              effect_fn,
              &[Arg { value, spread: false, functions: vec![], kind: ArgKind::Known }],
              Reach::NONE,
            );
          } else if let Some(api) = self.shared().api_of(self.module, other).map(str::to_string) {
            // A data-last function passed itself, e.g. `pipe(x, Effect.orDie)`.
            value = self.apply_api(&api, Some(value), vec![], other.span());
          } else {
            let applied = self.eval(other);
            value = self.call_unknown(
              applied,
              &[Arg { value, spread: false, functions: vec![], kind: ArgKind::Known }],
              other.span(),
            );
          }
        }
      }
    }
    value
  }

  /// A data-last call of an Effect API applied to `piped`, derived from the
  /// API's definition as the data-first call when the evaluator covers it.
  fn apply_api_or_derive(
    &mut self,
    callee: &Expression<'a>,
    api: &str,
    piped: Reach,
    args: Vec<Arg>,
    span: Span,
  ) -> Reach {
    if let Some(Resolved::Symbol(symbol)) = self.shared().resolve_expr(self.module, strip(callee)) {
      let mut full =
        vec![Arg { value: piped, spread: false, functions: vec![], kind: ArgKind::Known }];
      full.extend(args.iter().map(|arg| Arg {
        value: arg.value,
        spread: arg.spread,
        functions: arg.functions.clone(),
        kind: arg.kind,
      }));
      if let Some(result) = self.derive_api(symbol, &full) {
        self.state.derived.insert(api.to_string());
        return result;
      }
    }
    self.state.underived.insert(api.to_string());
    self.apply_api(api, Some(piped), args, span)
  }

  /// A call of an Effect API: `piped` is the value a data-last call receives
  /// through `pipe`.
  fn apply_api(
    &mut self,
    api: &str,
    piped: Option<Reach>,
    mut args: Vec<Arg>,
    span: Span,
  ) -> Reach {
    if let Some(value) = piped {
      args.insert(0, Arg { value, spread: false, functions: vec![], kind: ArgKind::Known });
    }
    let all = Self::join_all(&args);
    let first = args.first().map_or(Reach::NONE, |arg| arg.value);
    let source_module = SOURCE_MODULES.iter().any(|prefix| api.starts_with(prefix));
    if CALLBACK_SOURCES.contains(&api) || source_module {
      self.call_back(&args, Reach::TOP);
    }
    if RESULT_SOURCES.contains(&api) || source_module {
      if !CALLBACK_SOURCES.contains(&api) {
        self.call_back(&args, all.collapse());
      }
      return Reach::TOP;
    }
    // A key created outside a declaration: yielding it gives a service.
    if matches!(api, "Context.Service" | "Context.Reference") {
      self.call_back(&args, all.collapse());
      return Reach(0b110);
    }
    let _ = span;
    match api {
      "Effect.gen" => {
        let ret = self.call_back(&args, Reach::NONE);
        ret.effect_of()
      }
      "Effect.fn" | "Effect.fnUntraced" => {
        // Calls of the returned function are followed through its binding;
        // escaped ones may receive anything unknown code passes.
        let esc = self.state.esc_params;
        let ret = self.call_back(&args, esc);
        let _ = ret;
        Reach::NONE
      }
      "Effect.succeed" | "Effect.succeedSome" => first.effect_of(),
      "Effect.sync" | "Effect.promise" | "Effect.tryPromise" | "Effect.try" | "Effect.suspend" => {
        let ret = self.call_back(&args, Reach::NONE);
        if api == "Effect.suspend" { ret } else { ret.awaited().effect_of() }
      }
      "Effect.map" | "Effect.as" | "Effect.mapBoth" | "Effect.andThen" | "Effect.flatMap"
      | "Effect.tap" => {
        let input = first.success();
        let mut callback_args: Vec<Arg> = vec![];
        for arg in args.iter().skip(1) {
          callback_args.push(Arg {
            value: arg.value,
            spread: arg.spread,
            functions: arg.functions.clone(),
            kind: arg.kind,
          });
        }
        let ret = self.call_back(&callback_args, input);
        // Values passed instead of a callback (`Effect.as(e, value)`).
        let values = callback_args
          .iter()
          .filter(|arg| arg.functions.is_empty() && arg.kind == ArgKind::Known)
          .fold(Reach::NONE, |acc, arg| acc.join(arg.value));
        match api {
          "Effect.map" | "Effect.as" | "Effect.mapBoth" => ret.effect_of().join(values.effect_of()),
          "Effect.tap" => first,
          _ => ret.success().effect_of().join(values.success().effect_of()),
        }
      }
      "Effect.all" | "Effect.allSuccesses" => Reach(all.element().0 & 0b110),
      "Effect.forEach" => {
        let items = first.element();
        let ret = self.call_back(&args, items);
        Reach(ret.0 & 0b110)
      }
      "Effect.forkChild" | "Effect.forkIn" | "Effect.forkScoped" | "Effect.forkDetach" => {
        self.call_back(&args, all.collapse());
        first.fiber_of().effect_of()
      }
      "Effect.runFork" | "Effect.runForkWith" => {
        self.call_back(&args, all.collapse());
        first.fiber_of()
      }
      "Fiber.join" | "Fiber.await" | "Fiber.joinAll" | "Fiber.awaitAll" => {
        first.fiber_success().effect_of()
      }
      "Effect.runPromise"
      | "Effect.runPromiseExit"
      | "Effect.runPromiseWith"
      | "Effect.runPromiseExitWith" => {
        self.call_back(&args, all.collapse());
        Reach(first.0 & 0b110)
      }
      "Effect.runSync" | "Effect.runSyncExit" | "Effect.runSyncWith" | "Effect.runSyncExitWith" => {
        self.call_back(&args, all.collapse());
        self.async_fiber_errors();
        first.success()
      }
      "Effect.fail" | "Effect.die" | "Effect.failSync" | "Effect.dieSync" | "Effect.failCause" => {
        let ret = self.call_back(&args, Reach::NONE);
        let mut changed = self.state.changed;
        ReachState::join_field(&mut self.state.thrown, all.join(ret).collapse(), &mut changed);
        self.state.changed = changed;
        Reach::NONE
      }
      "Effect.provide" | "Effect.provideService" | "Effect.provideServiceEffect" => {
        self.call_back(&args, all.collapse());
        first
      }
      _ => {
        // Every other Effect API passes on what it was given; error handlers
        // also receive failures and defects.
        let lower = api.to_ascii_lowercase();
        let errors = [
          "catch",
          "orelse",
          "error",
          "cause",
          "match",
          "exit",
          "result",
          "either",
          "retry",
          "sandbox",
          "option",
          "ignore",
          "fail",
          "die",
          "ensuring",
          "finalizer",
          "release",
        ]
        .iter()
        .any(|word| lower.contains(word));
        let param = if errors { all.collapse().join(self.state.thrown) } else { all.collapse() };
        let ret = self.call_back(&args, param);
        all.join(ret).collapse()
      }
    }
  }
}

impl<'a> VisitJs<'a> for Walker<'_, '_, '_, 'a> {
  fn visit_expression(&mut self, it: &Expression<'a>) {
    self.eval(it);
  }

  fn visit_function(&mut self, it: &Function<'a>, _flags: ScopeFlags) {
    let id = (self.module, it.span.start);
    self.functions.push(id);
    self.bind_params(id, &it.params);
    if let Some(body) = &it.body {
      self.visit_function_body(body);
    }
    self.functions.pop();
  }

  fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
    let id = (self.module, it.span.start);
    self.functions.push(id);
    self.bind_params(id, &it.params);
    if let Some(expression) = it.get_expression() {
      let value = self.eval(expression);
      self.state.join_ret(id, value);
    } else if let ArrowFunctionBody::FunctionBody(body) = &it.body {
      self.visit_function_body(body);
    }
    self.functions.pop();
  }

  fn visit_class(&mut self, it: &Class<'a>) {
    if let Some(heritage) = &it.heritage {
      self.eval(&heritage.expression);
    }
    for element in &it.body.body {
      match element {
        ClassElement::MethodDefinition(method) => {
          if method.computed {
            if let Some(key) = method.key.as_expression() {
              self.eval(key);
            }
          }
          let id = (self.module, method.value.span.start);
          self.state.escape(id);
          self.visit_function(&method.value, ScopeFlags::Function);
        }
        ClassElement::PropertyDefinition(property) => {
          if property.computed {
            if let Some(key) = property.key.as_expression() {
              self.eval(key);
            }
          }
          if let Some(value) = &property.value {
            let value = self.eval(value);
            if let Some(name) = key_name(&property.key) {
              self.state.join_prop(&name, value);
            }
            let mut changed = self.state.changed;
            ReachState::join_field(&mut self.state.this_value, value, &mut changed);
            self.state.changed = changed;
          }
        }
        ClassElement::StaticBlock(block) => {
          for statement in &block.body {
            self.visit_statement(statement);
          }
        }
        ClassElement::AccessorProperty(property) => {
          if let Some(value) = &property.value {
            self.eval(value);
          }
        }
        ClassElement::TSIndexSignature(_) => {}
      }
    }
  }

  fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
    let guard = self.input.guard_declarators.get(&(self.module, it.span.start)).copied();
    if let Some(symbol) = guard {
      self.guards.push(Guard::Binding(symbol));
    }
    let value = match &it.init {
      Some(init) => self.eval(init),
      None => Reach::NONE,
    };
    if guard.is_some() {
      self.guards.pop();
    }
    self.bind_pattern(&it.id, value);
  }

  fn visit_return_statement(&mut self, it: &ReturnStatement<'a>) {
    let value = it.argument.as_ref().map_or(Reach::NONE, |argument| self.eval(argument));
    if let Some(function) = self.functions.last().copied() {
      self.state.join_ret(function, value);
    }
  }

  fn visit_throw_statement(&mut self, it: &ThrowStatement<'a>) {
    let value = self.eval(&it.argument);
    if value.any() {
      self.origin("thrown (throw)", it.span);
    }
    let mut changed = self.state.changed;
    ReachState::join_field(&mut self.state.thrown, value.collapse(), &mut changed);
    self.state.changed = changed;
  }

  fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
    if let Some(param) = &it.param {
      // Errors from code the analysis cannot see may hold anything it holds.
      let value = self.state.thrown.join(self.state.esc_params);
      self.bind_pattern(&param.pattern, value);
    }
    self.visit_block_statement(&it.body);
  }

  fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
    let iterable = self.eval(&it.right);
    let mut element = iterable.element();
    if it.r#await {
      element = element.awaited();
    }
    self.bind_for_left(&it.left, element);
    self.visit_statement(&it.body);
  }

  fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
    let object = self.eval(&it.right);
    if object.value() {
      self.event(ReachEvent::All("enumerated with for...in"), it.span);
    }
    self.bind_for_left(&it.left, Reach::NONE);
    self.visit_statement(&it.body);
  }

  fn visit_with_statement(&mut self, it: &WithStatement<'a>) {
    let object = self.eval(&it.object);
    self.observe_all(object, it.span, "used as a `with` scope");
    self.visit_statement(&it.body);
  }

  fn visit_statement(&mut self, it: &Statement<'a>) {
    walk_js::walk_statement(self, it);
  }
}

impl<'a> Walker<'_, '_, '_, 'a> {
  fn bind_for_left(&mut self, left: &ForStatementLeft<'a>, value: Reach) {
    match left {
      ForStatementLeft::VariableDeclaration(declaration) => {
        for declarator in &declaration.declarations {
          self.bind_pattern(&declarator.id, value);
        }
      }
      _ => {
        if let Some(target) = left.as_assignment_target() {
          self.assign_target(target, value);
        }
      }
    }
  }
}

// == Effect's own code
//
// A call of an Effect API is evaluated through the API's definition in
// Effect's modules, at the call site, with the call's arguments. The
// evaluator interprets the JavaScript of those definitions: constants,
// closures and their frames, objects and their fields, `this`, `new` with
// prototypes, `arguments`, conditionals, `switch`, `try`. Calls of the
// program's own functions go through the analysis above.
//
// Only what the fiber does is modelled, as the fiber interprets primitive
// operations and never reads a Context itself: the primitives are recognised
// by the `op` they are declared with (`makePrimitive`, `makeExit` and
// `makePrimitiveProto` in `internal/core.js`), and the fiber methods a
// continuation calls (`succeedWith`, `yieldWith`) return the Effect they are
// given. A `withFiber` closure receives the fiber, whose `context` holds every
// service.
//
// A definition the evaluator does not cover yields `None`, and the call falls
// back to the treatment of an unanalysed API.

/// Module-level definitions of Effect's modules, by canonical binding.
#[derive(Default)]
pub(super) struct EffectDefs<'s, 'a> {
  defs: FxHashMap<SymbolRef, (ModuleIdx, Def<'s, 'a>)>,
  /// `F.prototype = P` statements at module level.
  prototypes: FxHashMap<SymbolRef, (ModuleIdx, &'s Expression<'a>)>,
  /// Module-level values, evaluated once: `None` while in progress or when
  /// the evaluator does not cover the definition.
  values: std::cell::RefCell<FxHashMap<SymbolRef, Option<Value<'s, 'a>>>>,
  /// The keys under which a primitive stores its input and continuation
  /// (`args`, `contA` in `internal/core.js`).
  args_key: Option<String>,
  cont_key: Option<String>,
}

#[derive(Clone, Copy)]
enum Def<'s, 'a> {
  Expr(&'s Expression<'a>),
  Function(&'s Function<'a>),
}

impl<'s, 'a> EffectDefs<'s, 'a> {
  fn collect(&mut self, shared: &Shared<'_>, module: ModuleIdx, program: &'s Program<'a>) {
    let canonical =
      |symbol: oxc::semantic::SymbolId| shared.symbols.canonical_ref_for((module, symbol).into());
    for statement in &program.body {
      let declaration = match statement {
        Statement::ExportDeclaration(export) => Some(&export.declaration),
        Statement::ExpressionStatement(expression) => {
          // `F.prototype = P`.
          if let Expression::AssignmentExpression(assign) = &expression.expression {
            if let AssignmentTarget::StaticMemberExpression(member) = &assign.left {
              if let (Expression::Identifier(ident), "prototype") =
                (strip(&member.object), member.property.name.as_str())
              {
                if let Some(Resolved::Symbol(symbol)) = shared.resolve_ident(module, ident) {
                  self.prototypes.insert(symbol, (module, &assign.right));
                }
              }
            }
          }
          None
        }
        _ => statement.as_declaration(),
      };
      match declaration {
        Some(oxc::ast::ast::Declaration::VariableDeclaration(declaration))
          if declaration.kind == oxc::ast::ast::VariableDeclarationKind::Const =>
        {
          for declarator in &declaration.declarations {
            if let (BindingPattern::BindingIdentifier(id), Some(init)) =
              (&declarator.id, &declarator.init)
            {
              if let Some(symbol) = id.symbol_id.get() {
                self.defs.insert(canonical(symbol), (module, Def::Expr(init)));
              }
            }
          }
        }
        Some(oxc::ast::ast::Declaration::FunctionDeclaration(function)) => {
          if let Some(symbol) = function.id.as_ref().and_then(|id| id.symbol_id.get()) {
            self.defs.insert(canonical(symbol), (module, Def::Function(function)));
          }
        }
        _ => {}
      }
    }
  }

  fn find_keys(&mut self, shared: &Shared<'_>) {
    for (symbol, name) in &shared.api_names {
      match name.as_str() {
        "internal/core.args" => self.args_key = shared.const_strings.get(symbol).cloned(),
        "internal/core.contA" => self.cont_key = shared.const_strings.get(symbol).cloned(),
        _ => {}
      }
    }
  }
}

/// A primitive operation of the fiber, by the `op` it is declared with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Primitive {
  Success,
  Failure,
  WithFiber,
  WithFiberSucceed,
  Sync,
  Suspend,
  /// A success continuation: the fiber runs the input, then calls the
  /// continuation with its success.
  OnSuccess,
}

#[derive(Clone, Debug, PartialEq)]
enum Constant {
  Undefined,
  Null,
  Bool(bool),
  Number(f64),
  Str(String),
}

impl Constant {
  fn truthy(&self) -> bool {
    match self {
      Constant::Undefined | Constant::Null => false,
      Constant::Bool(value) => *value,
      Constant::Number(value) => *value != 0.0 && !value.is_nan(),
      Constant::Str(value) => !value.is_empty(),
    }
  }
  fn type_name(&self) -> &'static str {
    match self {
      Constant::Undefined => "undefined",
      Constant::Null => "object",
      Constant::Bool(_) => "boolean",
      Constant::Number(_) => "number",
      Constant::Str(_) => "string",
    }
  }
  fn key(&self) -> Option<String> {
    match self {
      Constant::Str(value) => Some(value.clone()),
      #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "non-negative integral property indices only"
      )]
      Constant::Number(value) if value.fract() == 0.0 && *value >= 0.0 => {
        Some((*value as u64).to_string())
      }
      _ => None,
    }
  }
}

type ObjectRef<'s, 'a> = Rc<std::cell::RefCell<Object<'s, 'a>>>;

#[derive(Default)]
struct Object<'s, 'a> {
  fields: FxHashMap<String, Value<'s, 'a>>,
  /// Values stored under keys that are not constant.
  other: Value<'s, 'a>,
  primitive: Option<Primitive>,
}

#[derive(Clone)]
enum Callable<'s, 'a> {
  /// A function of the program, followed by the analysis above.
  Program(FnId),
  /// A function of Effect's modules, with the frame it was created in and
  /// the binding it is declared as, for its prototype.
  Closure {
    module: ModuleIdx,
    params: &'s FormalParameters<'a>,
    body: ClosureBody<'s, 'a>,
    frame: Rc<Frame<'s, 'a>>,
    symbol: Option<SymbolRef>,
    /// An arrow's `this` and `arguments`, from where it was created.
    lexical: Option<(Box<Value<'s, 'a>>, Rc<[Value<'s, 'a>]>)>,
  },
  /// A primitive constructor (`makePrimitive`, `makeExit`).
  Primitive(Primitive),
  /// A method of the fiber, read in a continuation.
  FiberMethod(String),
}

#[derive(Clone, Copy)]
enum ClosureBody<'s, 'a> {
  Expr(&'s Expression<'a>),
  Block(&'s oxc::ast::ast::FunctionBody<'a>),
}

#[derive(Default)]
struct Frame<'s, 'a> {
  vars: std::cell::RefCell<FxHashMap<(ModuleIdx, oxc::semantic::SymbolId), Value<'s, 'a>>>,
  parent: Option<Rc<Frame<'s, 'a>>>,
}

impl<'s, 'a> Frame<'s, 'a> {
  fn child(parent: &Rc<Frame<'s, 'a>>) -> Rc<Frame<'s, 'a>> {
    Rc::new(Frame { vars: std::cell::RefCell::default(), parent: Some(Rc::clone(parent)) })
  }
  fn get(&self, key: (ModuleIdx, oxc::semantic::SymbolId)) -> Option<Value<'s, 'a>> {
    if let Some(value) = self.vars.borrow().get(&key) {
      return Some(value.clone());
    }
    self.parent.as_ref().and_then(|parent| parent.get(key))
  }
  fn declare(&self, key: (ModuleIdx, oxc::semantic::SymbolId), value: Value<'s, 'a>) {
    self.vars.borrow_mut().insert(key, value);
  }
  /// An assignment: joined into the binding wherever it is declared.
  fn assign(&self, key: (ModuleIdx, oxc::semantic::SymbolId), value: Value<'s, 'a>) -> bool {
    if let Some(current) = self.vars.borrow_mut().get_mut(&key) {
      *current = std::mem::take(current).join(value);
      return true;
    }
    self.parent.as_ref().is_some_and(|parent| parent.assign(key, value))
  }
}

/// A value inside Effect's code.
#[derive(Clone, Default)]
struct Value<'s, 'a> {
  /// What the program's values in it may reach.
  reach: Reach,
  callables: Vec<Callable<'s, 'a>>,
  objects: Vec<ObjectRef<'s, 'a>>,
  constant: Option<Constant>,
  /// The running fiber.
  fiber: bool,
  /// `arguments` of the call being evaluated.
  arguments: Option<Rc<[Value<'s, 'a>]>>,
  /// `makePrimitiveProto({ op })`.
  primitive_proto: Option<Primitive>,
  /// Not known: usable only to choose a branch or as a discarded result.
  unknown: bool,
  /// May be a function the analysis cannot see: calling it is not followed.
  opaque_callee: bool,
}

impl<'s, 'a> Value<'s, 'a> {
  fn reach(reach: Reach) -> Self {
    Value { reach, ..Value::default() }
  }
  fn constant(constant: Constant) -> Self {
    Value { constant: Some(constant), ..Value::default() }
  }
  fn unknown() -> Self {
    Value { unknown: true, ..Value::default() }
  }
  fn join(mut self, other: Value<'s, 'a>) -> Self {
    self.reach = self.reach.join(other.reach);
    self.callables.extend(other.callables);
    for object in other.objects {
      if !self.objects.iter().any(|known| Rc::ptr_eq(known, &object)) {
        self.objects.push(object);
      }
    }
    if self.constant != other.constant {
      self.constant = None;
    }
    self.fiber |= other.fiber;
    if !matches!((&self.arguments, &other.arguments), (Some(a), Some(b)) if Rc::ptr_eq(a, b)) {
      self.arguments = None;
    }
    if self.primitive_proto != other.primitive_proto {
      self.primitive_proto = None;
    }
    self.unknown |= other.unknown;
    self.opaque_callee |= other.opaque_callee;
    self
  }
  /// Whether the value may carry something of the program.
  fn carries_program(&self) -> bool {
    self.carries_program_within(&mut vec![], 0)
  }
  /// Whether writing into the value may write into the program's values.
  fn carries_program_shallow_target(&self) -> bool {
    self.reach.any()
      || self.opaque_callee
      || self.callables.iter().any(|callable| matches!(callable, Callable::Program(_)))
  }
  fn carries_program_within(
    &self,
    seen: &mut Vec<*const std::cell::RefCell<Object<'s, 'a>>>,
    depth: usize,
  ) -> bool {
    if self.reach.any()
      || self.fiber
      || self.opaque_callee
      || self.callables.iter().any(|callable| matches!(callable, Callable::Program(_)))
      || self.arguments.as_ref().is_some_and(|arguments| {
        arguments.iter().any(|argument| argument.carries_program_within(seen, depth + 1))
      })
    {
      return true;
    }
    if depth > DEFINITION_DEPTH {
      return true;
    }
    self.objects.iter().any(|object| {
      if seen.contains(&Rc::as_ptr(object)) {
        return false;
      }
      seen.push(Rc::as_ptr(object));
      let object = object.borrow();
      object.fields.values().any(|field| field.carries_program_within(seen, depth + 1))
        || object.other.carries_program_within(seen, depth + 1)
    })
  }
}

/// The evaluation context of a call in Effect's code.
struct Ctx<'s, 'a> {
  module: ModuleIdx,
  frame: Rc<Frame<'s, 'a>>,
  this: Value<'s, 'a>,
  arguments: Rc<[Value<'s, 'a>]>,
}

/// How a statement list ends.
#[derive(Default)]
struct Flow<'s, 'a> {
  returns: Value<'s, 'a>,
  /// May run past its end.
  falls_through: bool,
  /// Ended by `break`.
  broke: bool,
}

const DEFINITION_DEPTH: usize = 48;

impl<'s, 'a> Walker<'_, 's, '_, 'a> {
  /// The result of calling the Effect API bound to `api` with `args`, derived
  /// from its definition; `None` when the definition is outside what the
  /// evaluator covers.
  fn derive_api(&mut self, api: SymbolRef, args: &[Arg]) -> Option<Reach> {
    let key: DeriveKey =
      (api, args.iter().map(|arg| (arg.value.0, arg.functions.clone())).collect());
    if self.state.derive_failures.contains(&key) {
      return None;
    }
    if !self.state.final_walk {
      if let Some(cached) = self.state.derive_cache.get(&key) {
        return *cached;
      }
    }
    let result = self.derive_api_uncached(api, args);
    if result.is_none() {
      self.state.derive_failures.insert(key.clone());
    }
    self.state.derive_cache.insert(key, result);
    result
  }

  fn derive_api_uncached(&mut self, api: SymbolRef, args: &[Arg]) -> Option<Reach> {
    let mut values = vec![];
    for arg in args {
      // A spread, or a curried keyed lookup, is not followed into Effect's
      // code. A value that may be a function the analysis cannot see stops
      // the derivation only if Effect's code calls it.
      if arg.spread || arg.kind == ArgKind::Lookup {
        return None;
      }
      values.push(Value {
        reach: arg.value,
        callables: arg.functions.iter().map(|function| Callable::Program(*function)).collect(),
        opaque_callee: matches!(arg.kind, ArgKind::Host | ArgKind::Unknown),
        ..Value::default()
      });
    }
    let callee = self.def_value(api, 0)?;
    let result = self.call_value_def(&callee, &Value::default(), &values, 0)?;
    if result.unknown {
      return None;
    }
    self.to_reach(&result, 0)
  }

  /// The value a module-level definition of Effect's modules evaluates to.
  fn def_value(&mut self, symbol: SymbolRef, depth: usize) -> Option<Value<'s, 'a>> {
    if depth > DEFINITION_DEPTH {
      return None;
    }
    if let Some(cached) = self.defs.values.borrow().get(&symbol) {
      return cached.clone();
    }
    let (module, def) = *self.defs.defs.get(&symbol)?;
    self.defs.values.borrow_mut().insert(symbol, None);
    let value = match def {
      Def::Function(function) => function.body.as_deref().map(|body| Value {
        callables: vec![Callable::Closure {
          module,
          params: &function.params,
          body: ClosureBody::Block(body),
          frame: Rc::default(),
          symbol: Some(symbol),
          lexical: None,
        }],
        ..Value::default()
      }),
      Def::Expr(init) => match self.primitive_declaration(module, init) {
        Some(value) => Some(value),
        None => {
          let ctx =
            Ctx { module, frame: Rc::default(), this: Value::default(), arguments: Rc::default() };
          self.eval_def(&ctx, init, depth + 1).map(|mut value| {
            for callable in &mut value.callables {
              if let Callable::Closure { symbol: slot @ None, .. } = callable {
                *slot = Some(symbol);
              }
            }
            value
          })
        }
      },
    };
    self.defs.values.borrow_mut().insert(symbol, value.clone());
    value
  }

  /// `makePrimitive({ op })`, `makeExit({ op })` or `makePrimitiveProto({ op })`.
  fn primitive_declaration(
    &self,
    module: ModuleIdx,
    expr: &Expression<'_>,
  ) -> Option<Value<'s, 'a>> {
    let Expression::CallExpression(call) = strip(expr) else { return None };
    let api = self.shared().api_of(module, &call.callee)?;
    let proto = match api {
      "internal/core.makePrimitive" | "internal/core.makeExit" => false,
      "internal/core.makePrimitiveProto" => true,
      _ => return None,
    };
    let Some(Expression::ObjectExpression(options)) =
      call.arguments.first().and_then(Argument::as_expression).map(strip)
    else {
      return None;
    };
    let op = options.properties.iter().find_map(|property| match property {
      ObjectPropertyKind::ObjectProperty(property)
        if !property.computed && key_name(&property.key).as_deref() == Some("op") =>
      {
        match strip(&property.value) {
          Expression::StringLiteral(op) => op.value.as_str().map(String::from),
          _ => None,
        }
      }
      _ => None,
    })?;
    let primitive = match op.as_str() {
      "Success" => Primitive::Success,
      "Failure" => Primitive::Failure,
      "WithFiber" => Primitive::WithFiber,
      "WithFiberSucceed" => Primitive::WithFiberSucceed,
      "Sync" => Primitive::Sync,
      "Suspend" => Primitive::Suspend,
      "OnSuccess" => Primitive::OnSuccess,
      _ => return None,
    };
    Some(if proto {
      Value { primitive_proto: Some(primitive), ..Value::default() }
    } else {
      Value { callables: vec![Callable::Primitive(primitive)], ..Value::default() }
    })
  }

  fn closure(
    ctx: &Ctx<'s, 'a>,
    params: &'s FormalParameters<'a>,
    body: ClosureBody<'s, 'a>,
    arrow: bool,
  ) -> Value<'s, 'a> {
    Value {
      callables: vec![Callable::Closure {
        module: ctx.module,
        params,
        body,
        frame: Rc::clone(&ctx.frame),
        symbol: None,
        lexical: arrow.then(|| (Box::new(ctx.this.clone()), Rc::clone(&ctx.arguments))),
      }],
      ..Value::default()
    }
  }

  #[expect(clippy::too_many_lines, reason = "one arm per expression kind")]
  fn eval_def(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    expr: &'s Expression<'a>,
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    if depth > DEFINITION_DEPTH {
      return None;
    }
    let depth = depth + 1;
    Some(match expr {
      Expression::ParenthesizedExpression(paren) => {
        return self.eval_def(ctx, &paren.expression, depth);
      }
      Expression::TSAsExpression(e) => return self.eval_def(ctx, &e.expression, depth),
      Expression::TSNonNullExpression(e) => return self.eval_def(ctx, &e.expression, depth),
      Expression::StringLiteral(literal) => {
        Value::constant(Constant::Str(literal.value.as_str().unwrap_or_default().to_string()))
      }
      Expression::NumericLiteral(literal) => Value::constant(Constant::Number(literal.value)),
      Expression::BooleanLiteral(literal) => Value::constant(Constant::Bool(literal.value)),
      Expression::NullLiteral(_) => Value::constant(Constant::Null),
      Expression::BigIntLiteral(_) | Expression::RegExpLiteral(_) => Value::default(),
      Expression::TemplateLiteral(template) => {
        let mut text = Some(String::new());
        for (index, quasi) in template.quasis.iter().enumerate() {
          if let (Some(out), Some(cooked)) = (text.as_mut(), quasi.value.cooked.as_ref()) {
            out.push_str(cooked.as_str().unwrap_or_default());
          }
          if let Some(expression) = template.expressions.get(index) {
            let value = self.eval_def(ctx, expression, depth)?;
            if value.carries_program() {
              self.observe_all(
                Reach::TOP,
                expression.span(),
                "converted to a string in Effect's code",
              );
            }
            match (&mut text, value.constant.as_ref().and_then(Constant::key)) {
              (Some(out), Some(part)) => out.push_str(&part),
              _ => text = None,
            }
          }
        }
        text.map_or_else(Value::unknown, |text| Value::constant(Constant::Str(text)))
      }
      Expression::Identifier(ident) => return self.eval_def_identifier(ctx, ident, depth),
      Expression::ThisExpression(_) => ctx.this.clone(),
      Expression::StaticMemberExpression(member) => {
        if matches!(
          self.shared().resolve_expr(ctx.module, &member.object),
          Some(Resolved::Namespace(_))
        ) {
          let Some(Resolved::Symbol(symbol)) = self.shared().resolve_expr(ctx.module, expr) else {
            return None;
          };
          return self.def_value(symbol, depth);
        }
        let object = self.eval_def(ctx, &member.object, depth)?;
        self.member_def(&object, Some(member.property.name.as_str()), member.span)
      }
      Expression::ComputedMemberExpression(member) => {
        let object = self.eval_def(ctx, &member.object, depth)?;
        let key = self.eval_def(ctx, &member.expression, depth)?;
        let key = key.constant.as_ref().and_then(Constant::key);
        self.member_def(&object, key.as_deref(), member.span)
      }
      Expression::ChainExpression(chain) => match &chain.expression {
        ChainElement::CallExpression(call) => return self.eval_def_call(ctx, call, depth),
        ChainElement::StaticMemberExpression(member) => {
          let object = self.eval_def(ctx, &member.object, depth)?;
          self.member_def(&object, Some(member.property.name.as_str()), member.span)
        }
        ChainElement::ComputedMemberExpression(member) => {
          let object = self.eval_def(ctx, &member.object, depth)?;
          let key = self.eval_def(ctx, &member.expression, depth)?;
          let key = key.constant.as_ref().and_then(Constant::key);
          self.member_def(&object, key.as_deref(), member.span)
        }
        _ => return None,
      },
      Expression::ArrowFunctionExpression(arrow) => {
        let body = match arrow.get_expression() {
          Some(expression) => ClosureBody::Expr(expression),
          None => match &arrow.body {
            ArrowFunctionBody::FunctionBody(body) => ClosureBody::Block(body),
            _ => return None,
          },
        };
        Self::closure(ctx, &arrow.params, body, true)
      }
      Expression::FunctionExpression(function) => {
        let body = function.body.as_deref()?;
        Self::closure(ctx, &function.params, ClosureBody::Block(body), false)
      }
      Expression::ObjectExpression(object) => {
        let mut data = Object::default();
        for property in &object.properties {
          let ObjectPropertyKind::ObjectProperty(property) = property else { return None };
          let key = if property.computed {
            let key = self.eval_def(ctx, property.key.as_expression()?, depth)?;
            key.constant.as_ref().and_then(Constant::key)
          } else {
            key_name(&property.key)
          };
          let value = self.eval_def(ctx, &property.value, depth)?;
          match key {
            Some(key) => {
              data.fields.insert(key, value);
            }
            None => data.other = std::mem::take(&mut data.other).join(value),
          }
        }
        Value { objects: vec![Rc::new(std::cell::RefCell::new(data))], ..Value::default() }
      }
      Expression::ArrayExpression(array) => {
        let mut data = Object::default();
        for element in &array.elements {
          let value = match element {
            ArrayExpressionElement::SpreadElement(spread) => {
              let value = self.eval_def(ctx, &spread.argument, depth)?;
              self.element_def(&value)
            }
            ArrayExpressionElement::Elision(_) => Value::constant(Constant::Undefined),
            element => self.eval_def(ctx, element.to_expression(), depth)?,
          };
          data.other = std::mem::take(&mut data.other).join(value);
        }
        Value { objects: vec![Rc::new(std::cell::RefCell::new(data))], ..Value::default() }
      }
      Expression::ConditionalExpression(conditional) => {
        match self.eval_test(ctx, &conditional.test, depth) {
          Some(true) => return self.eval_def(ctx, &conditional.consequent, depth),
          Some(false) => return self.eval_def(ctx, &conditional.alternate, depth),
          None => {
            let consequent = self.eval_def(ctx, &conditional.consequent, depth)?;
            let alternate = self.eval_def(ctx, &conditional.alternate, depth)?;
            consequent.join(alternate)
          }
        }
      }
      Expression::LogicalExpression(logical) => {
        let left = self.eval_def(ctx, &logical.left, depth).unwrap_or_else(Value::unknown);
        let decided = left.constant.as_ref().map(Constant::truthy);
        let right_needed = match (logical.operator, decided) {
          (oxc::ast::ast::LogicalOperator::And, Some(false))
          | (oxc::ast::ast::LogicalOperator::Or, Some(true)) => false,
          (oxc::ast::ast::LogicalOperator::Coalesce, _) => {
            !matches!(left.constant, Some(ref c) if !matches!(c, Constant::Undefined | Constant::Null))
          }
          _ => true,
        };
        if !right_needed {
          return Some(left);
        }
        let right = self.eval_def(ctx, &logical.right, depth)?;
        match (logical.operator, decided) {
          (oxc::ast::ast::LogicalOperator::And, Some(true))
          | (oxc::ast::ast::LogicalOperator::Or, Some(false)) => right,
          _ => {
            if left.unknown && left.carries_program() {
              return None;
            }
            Value { unknown: false, ..left }.join(right)
          }
        }
      }
      Expression::BinaryExpression(binary) => self.eval_def_binary(ctx, binary, depth),
      Expression::UnaryExpression(unary) => {
        let value = self.eval_def(ctx, &unary.argument, depth).unwrap_or_else(Value::unknown);
        match unary.operator {
          UnaryOperator::Typeof => {
            match (&value.constant, value.callables.is_empty(), value.objects.is_empty()) {
              (Some(constant), _, _) => {
                Value::constant(Constant::Str(constant.type_name().to_string()))
              }
              (None, false, true) if !value.unknown && !value.reach.any() => {
                Value::constant(Constant::Str("function".to_string()))
              }
              _ => Value::unknown(),
            }
          }
          UnaryOperator::LogicalNot => match &value.constant {
            Some(constant) => Value::constant(Constant::Bool(!constant.truthy())),
            None => Value::unknown(),
          },
          UnaryOperator::Void => Value::constant(Constant::Undefined),
          _ => {
            if value.carries_program() {
              self.observe_all(Reach::TOP, unary.span, "converted in Effect's code");
            }
            Value::unknown()
          }
        }
      }
      Expression::SequenceExpression(sequence) => {
        let mut last = Value::default();
        for expression in &sequence.expressions {
          last = self.eval_def(ctx, expression, depth)?;
        }
        last
      }
      Expression::AssignmentExpression(assign) => {
        let value = self.eval_def(ctx, &assign.right, depth)?;
        if !assign.operator.is_assign() {
          return None;
        }
        self.assign_def(ctx, &assign.left, &value, depth)?;
        value
      }
      Expression::CallExpression(call) => return self.eval_def_call(ctx, call, depth),
      Expression::NewExpression(new) => return self.eval_def_new(ctx, new, depth),
      _ => return None,
    })
  }

  fn eval_def_identifier(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    ident: &oxc::ast::ast::IdentifierReference<'a>,
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    let scoping = self.shared().scoping(ctx.module);
    let symbol = scoping.get_reference(ident.reference_id.get()?).symbol_id();
    match symbol {
      Some(symbol) => {
        if let Some(value) = ctx.frame.get((ctx.module, symbol)) {
          return Some(value);
        }
        let canonical = self.shared().symbols.canonical_ref_for((ctx.module, symbol).into());
        if let Some(text) = self.shared().const_strings.get(&canonical) {
          return Some(Value::constant(Constant::Str(text.clone())));
        }
        self.def_value(canonical, depth)
      }
      None => Some(match ident.name.as_str() {
        "undefined" => Value::constant(Constant::Undefined),
        "arguments" => Value { arguments: Some(Rc::clone(&ctx.arguments)), ..Value::default() },
        // Builtins: usable to choose a branch, not to carry the program's values.
        _ => Value::unknown(),
      }),
    }
  }

  /// `value.key`, or `value[unknown]` when `key` is `None`.
  fn member_def(&mut self, object: &Value<'s, 'a>, key: Option<&str>, span: Span) -> Value<'s, 'a> {
    let mut out = Value::default();
    let mut found = false;
    if object.fiber {
      found = true;
      match key {
        // The fiber's Context holds every service.
        Some("context") => out = out.join(Value::reach(Reach::TOP)),
        Some(name) => {
          out = out.join(Value {
            callables: vec![Callable::FiberMethod(name.to_string())],
            unknown: true,
            ..Value::default()
          });
        }
        None => out = out.join(Value::unknown()),
      }
    }
    if let Some(arguments) = &object.arguments {
      found = true;
      match key {
        Some("length") => {
          #[expect(clippy::cast_precision_loss, reason = "argument counts are small")]
          let length = arguments.len() as f64;
          out = out.join(Value::constant(Constant::Number(length)));
        }
        Some(index) if index.parse::<usize>().is_ok() => {
          let index: usize = index.parse().unwrap_or_default();
          out = out.join(
            arguments.get(index).cloned().unwrap_or_else(|| Value::constant(Constant::Undefined)),
          );
        }
        _ => {
          for argument in arguments.iter() {
            out = out.join(argument.clone());
          }
        }
      }
    }
    for object in &object.objects {
      found = true;
      let object = object.borrow();
      let value = match key {
        Some(key) => object.fields.get(key).cloned().unwrap_or_default().join(object.other.clone()),
        None => {
          object.fields.values().fold(object.other.clone(), |acc, value| acc.join(value.clone()))
        }
      };
      out = out.join(value);
    }
    if object.reach.any() || object.callables.iter().any(|c| matches!(c, Callable::Program(_))) {
      found = true;
      // A value of the program, read by Effect's code.
      if object.reach.value() {
        match key {
          Some(name) => self.event(ReachEvent::Name(name.to_string()), span),
          None => self.event(ReachEvent::All("read by a computed name in Effect's code"), span),
        }
      }
      out = out.join(Value {
        reach: object.reach.member(),
        callables: object
          .callables
          .iter()
          .filter(|c| matches!(c, Callable::Program(_)))
          .cloned()
          .collect(),
        ..Value::default()
      });
    }
    if object.opaque_callee {
      found = true;
      out =
        out.join(Value { reach: object.reach.member(), opaque_callee: true, ..Value::default() });
    }
    if object.constant.is_some() || object.unknown || !found {
      out = out.join(Value::unknown());
    }
    out
  }

  /// An element of an iterable value.
  fn element_def(&self, value: &Value<'s, 'a>) -> Value<'s, 'a> {
    let mut out = Value { reach: value.reach.element(), ..Value::default() };
    if let Some(arguments) = &value.arguments {
      for argument in arguments.iter() {
        out = out.join(argument.clone());
      }
    }
    for object in &value.objects {
      let object = object.borrow();
      out = out.join(object.other.clone());
      for field in object.fields.values() {
        out = out.join(field.clone());
      }
    }
    if value.unknown || value.constant.is_some() {
      out = out.join(Value::unknown());
    }
    out
  }

  /// A condition: `Some` when it is decided.
  fn eval_test(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    expr: &'s Expression<'a>,
    depth: usize,
  ) -> Option<bool> {
    let value = self.eval_def(ctx, expr, depth)?;
    value.constant.as_ref().map(Constant::truthy)
  }

  fn eval_def_binary(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    binary: &'s oxc::ast::ast::BinaryExpression<'a>,
    depth: usize,
  ) -> Value<'s, 'a> {
    let left = self.eval_def(ctx, &binary.left, depth).unwrap_or_else(Value::unknown);
    let right = self.eval_def(ctx, &binary.right, depth).unwrap_or_else(Value::unknown);
    let both = |f: &dyn Fn(&Constant, &Constant) -> Option<Constant>| match (
      &left.constant,
      &right.constant,
    ) {
      (Some(a), Some(b)) => f(a, b).map(Value::constant).unwrap_or_else(Value::unknown),
      _ => Value::unknown(),
    };
    let number = |c: &Constant| match c {
      Constant::Number(n) => Some(*n),
      _ => None,
    };
    match binary.operator {
      BinaryOperator::StrictEquality => both(&|a, b| Some(Constant::Bool(a == b))),
      BinaryOperator::StrictInequality => both(&|a, b| Some(Constant::Bool(a != b))),
      BinaryOperator::GreaterEqualThan => {
        both(&|a, b| Some(Constant::Bool(number(a)? >= number(b)?)))
      }
      BinaryOperator::GreaterThan => both(&|a, b| Some(Constant::Bool(number(a)? > number(b)?))),
      BinaryOperator::LessEqualThan => both(&|a, b| Some(Constant::Bool(number(a)? <= number(b)?))),
      BinaryOperator::LessThan => both(&|a, b| Some(Constant::Bool(number(a)? < number(b)?))),
      BinaryOperator::In | BinaryOperator::Instanceof => {
        if right.reach.value() {
          match left.constant.as_ref().and_then(Constant::key) {
            Some(name) => self.event(ReachEvent::Name(name), binary.span),
            None => {
              self
                .event(ReachEvent::All("`in` with a computed name in Effect's code"), binary.span);
            }
          }
        }
        Value::unknown()
      }
      _ => {
        // Arithmetic, concatenation and loose comparisons convert their
        // operands.
        if left.carries_program() || right.carries_program() {
          self.observe_all(Reach::TOP, binary.span, "converted by an operator in Effect's code");
        }
        Value::unknown()
      }
    }
  }

  fn assign_def(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    target: &'s AssignmentTarget<'a>,
    value: &Value<'s, 'a>,
    depth: usize,
  ) -> Option<()> {
    match target {
      AssignmentTarget::AssignmentTargetIdentifier(ident) => {
        let scoping = self.shared().scoping(ctx.module);
        let symbol = scoping.get_reference(ident.reference_id.get()?).symbol_id();
        if symbol.is_some_and(|symbol| ctx.frame.assign((ctx.module, symbol), value.clone())) {
          return Some(());
        }
        // Module state of Effect's modules is not modelled: only Effect's own
        // values may be stored there.
        (!value.carries_program()).then_some(())
      }
      AssignmentTarget::StaticMemberExpression(member) => {
        let object = self.eval_def(ctx, &member.object, depth)?;
        self.store_def(&object, Some(member.property.name.as_str()), value)
      }
      AssignmentTarget::ComputedMemberExpression(member) => {
        let object = self.eval_def(ctx, &member.object, depth)?;
        let key = self.eval_def(ctx, &member.expression, depth)?;
        self.store_def(&object, key.constant.as_ref().and_then(Constant::key).as_deref(), value)
      }
      _ => None,
    }
  }

  /// Stores `value` in the objects `object` may be.
  fn store_def(
    &self,
    object: &Value<'s, 'a>,
    key: Option<&str>,
    value: &Value<'s, 'a>,
  ) -> Option<()> {
    // Into objects Effect's code built here. Anything else (the program's
    // values, the fiber, builtins) is not modelled, so only Effect's own
    // values may be stored there.
    if object.objects.is_empty() || object.fiber || object.reach.any() || object.unknown {
      return (!value.carries_program() && !object.carries_program_shallow_target()).then_some(());
    }
    for target in &object.objects {
      let mut target = target.borrow_mut();
      match key {
        Some(key) => {
          let current = target.fields.remove(key).unwrap_or_default();
          target.fields.insert(key.to_string(), current.join(value.clone()));
        }
        None => target.other = std::mem::take(&mut target.other).join(value.clone()),
      }
    }
    Some(())
  }

  fn eval_def_args(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    arguments: &'s ArenaVec<'a, Argument<'a>>,
    depth: usize,
  ) -> Option<Vec<Value<'s, 'a>>> {
    let mut out = vec![];
    for argument in arguments {
      match argument {
        Argument::SpreadElement(spread) => {
          let value = self.eval_def(ctx, &spread.argument, depth)?;
          match &value.arguments {
            Some(arguments) if value.objects.is_empty() => out.extend(arguments.iter().cloned()),
            _ => return None,
          }
        }
        argument => out.push(self.eval_def(ctx, argument.to_expression(), depth)?),
      }
    }
    Some(out)
  }

  fn eval_def_call(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    call: &'s CallExpression<'a>,
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    // `f.call(thisArg, ...)` and `f.apply(thisArg, arguments)`.
    if let Expression::StaticMemberExpression(member) = strip(&call.callee) {
      let name = member.property.name.as_str();
      if matches!(name, "call" | "apply") {
        let function = self.eval_def(ctx, &member.object, depth)?;
        if !function.callables.is_empty() && function.objects.is_empty() && !function.fiber {
          let mut args = self.eval_def_args(ctx, &call.arguments, depth)?;
          let this = if args.is_empty() { Value::default() } else { args.remove(0) };
          if name == "apply" {
            let list = args.into_iter().next().unwrap_or_default();
            args = {
              let arguments = &list.arguments?;
              arguments.to_vec()
            };
          }
          return self.call_value_def(&function, &this, &args, depth);
        }
      }
    }
    let (callee, this) = match strip(&call.callee) {
      Expression::StaticMemberExpression(member)
        if !matches!(
          self.shared().resolve_expr(ctx.module, &member.object),
          Some(Resolved::Namespace(_))
        ) =>
      {
        let object = self.eval_def(ctx, &member.object, depth)?;
        (self.member_def(&object, Some(member.property.name.as_str()), member.span), object)
      }
      Expression::ComputedMemberExpression(member) => {
        let object = self.eval_def(ctx, &member.object, depth)?;
        let key = self.eval_def(ctx, &member.expression, depth)?;
        let key = key.constant.as_ref().and_then(Constant::key);
        (self.member_def(&object, key.as_deref(), member.span), object)
      }
      callee => (self.eval_def(ctx, callee, depth)?, Value::default()),
    };
    let args = self.eval_def_args(ctx, &call.arguments, depth)?;
    self.call_value_def(&callee, &this, &args, depth)
  }

  fn eval_def_new(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    new: &'s oxc::ast::ast::NewExpression<'a>,
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    let callee = self.eval_def(ctx, &new.callee, depth)?;
    let args = self.eval_def_args(ctx, &new.arguments, depth)?;
    if callee.callables.is_empty() {
      // A builtin constructor (`Error`, `Map`, ...): it may only see Effect's
      // own values.
      if callee.unknown && !args.iter().any(Value::carries_program) {
        return Some(Value::unknown());
      }
      return None;
    }
    let mut out = Value::default();
    for callable in callee.callables.clone() {
      let Callable::Closure { symbol, .. } = &callable else { return None };
      let primitive = match symbol.and_then(|symbol| self.defs.prototypes.get(&symbol).copied()) {
        Some((module, proto)) => {
          let ctx =
            Ctx { module, frame: Rc::default(), this: Value::default(), arguments: Rc::default() };
          self.eval_def(&ctx, proto, depth)?.primitive_proto
        }
        None => None,
      };
      let object = Rc::new(std::cell::RefCell::new(Object { primitive, ..Object::default() }));
      let this = Value { objects: vec![object], ..Value::default() };
      let single = Value { callables: vec![callable], ..Value::default() };
      self.call_value_def(&single, &this, &args, depth)?;
      out = out.join(this);
    }
    Some(out)
  }

  /// Calls every function `callee` may be.
  fn call_value_def(
    &mut self,
    callee: &Value<'s, 'a>,
    this: &Value<'s, 'a>,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    if depth > DEFINITION_DEPTH || callee.opaque_callee {
      return None;
    }
    if callee.callables.is_empty() {
      // A builtin or an unknown function, called with Effect's own values
      // only: its result can choose a branch.
      if callee.unknown && !args.iter().any(Value::carries_program) && !this.carries_program() {
        return Some(Value::unknown());
      }
      return None;
    }
    let mut result = Value::default();
    for callable in callee.callables.clone() {
      let value = match callable {
        Callable::Program(function) => self.call_program_from_def(function, args)?,
        Callable::Closure { module, params, body, frame, lexical, .. } => {
          let frame = Frame::child(&frame);
          let (this, arguments) = match lexical {
            Some((this, arguments)) => (*this, arguments),
            None => (this.clone(), Rc::from(args.to_vec())),
          };
          for (index, param) in params.items.iter().enumerate() {
            let BindingPattern::BindingIdentifier(id) = &param.pattern else { return None };
            let mut value =
              args.get(index).cloned().unwrap_or_else(|| Value::constant(Constant::Undefined));
            if let Some(initializer) = &param.initializer {
              let ctx = Ctx {
                module,
                frame: Rc::clone(&frame),
                this: this.clone(),
                arguments: Rc::clone(&arguments),
              };
              value = value.join(self.eval_def(&ctx, initializer, depth + 1)?);
            }
            frame.declare((module, id.symbol_id.get()?), value);
          }
          if let Some(rest) = &params.rest {
            let BindingPattern::BindingIdentifier(id) = &rest.rest.argument else { return None };
            let mut data = Object::default();
            for arg in args.iter().skip(params.items.len()) {
              data.other = std::mem::take(&mut data.other).join(arg.clone());
            }
            frame.declare(
              (module, id.symbol_id.get()?),
              Value { objects: vec![Rc::new(std::cell::RefCell::new(data))], ..Value::default() },
            );
          }
          let ctx = Ctx { module, frame, this: this.clone(), arguments };
          match body {
            ClosureBody::Expr(expression) => self.eval_def(&ctx, expression, depth + 1)?,
            ClosureBody::Block(body) => {
              let flow = self.exec_def(&ctx, &body.statements, depth + 1)?;
              if flow.falls_through {
                flow.returns.join(Value::constant(Constant::Undefined))
              } else {
                flow.returns
              }
            }
          }
        }
        Callable::Primitive(primitive) => self.primitive(primitive, args, depth)?,
        Callable::FiberMethod(name) => match name.as_str() {
          // The current Effect succeeds with the value, or with the Exit.
          "succeedWith" => Value::reach(self.to_reach(args.first()?, depth)?.effect_of()),
          "yieldWith" => args.first()?.clone(),
          _ => return None,
        },
      };
      result = result.join(value);
    }
    Some(result)
  }

  /// Calls a function of the program from Effect's code.
  fn call_program_from_def(
    &mut self,
    function: FnId,
    args: &[Value<'s, 'a>],
  ) -> Option<Value<'s, 'a>> {
    let mut program_args = vec![];
    for arg in args {
      // Effect's own functions handed to the program are not followed. The
      // fiber is a runtime object: what the program reads from it is
      // analysed like any other read.
      if arg.callables.iter().any(|c| !matches!(c, Callable::Program(_))) {
        return None;
      }
      let value = self.to_reach(arg, 0)?;
      let mut functions: Vec<FnId> = arg
        .callables
        .iter()
        .filter_map(|c| match c {
          Callable::Program(function) => Some(*function),
          _ => None,
        })
        .collect();
      for object in &arg.objects {
        let object = object.borrow();
        for field in object.fields.values().chain(std::iter::once(&object.other)) {
          if field.callables.iter().any(|c| !matches!(c, Callable::Program(_))) {
            return None;
          }
          functions.extend(field.callables.iter().filter_map(|c| match c {
            Callable::Program(function) => Some(*function),
            _ => None,
          }));
        }
      }
      program_args.push(Arg { value, spread: false, functions, kind: ArgKind::Known });
    }
    let effect_fn = self.info.effect_generators.contains(&function);
    let ret = self.call_function(function, effect_fn, &program_args, Reach::NONE);
    // Functions the program returns are not followed back into Effect's code.
    Some(Value::reach(ret))
  }

  /// What a value of Effect's code means to the program: its reach, with
  /// Effects built from primitives read as the success they run to.
  fn to_reach(&mut self, value: &Value<'s, 'a>, depth: usize) -> Option<Reach> {
    if value.unknown || depth > DEFINITION_DEPTH {
      return None;
    }
    let mut reach = value.reach;
    if value.fiber {
      reach = Reach::TOP;
    }
    if let Some(arguments) = &value.arguments {
      for argument in arguments.iter() {
        reach = reach.join(self.to_reach(argument, depth + 1)?);
      }
    }
    for object in &value.objects {
      let (primitive, fields, other) = {
        let object = object.borrow();
        (object.primitive, object.fields.clone(), object.other.clone())
      };
      match primitive {
        Some(Primitive::OnSuccess) => {
          let input = fields.get(self.defs.args_key.as_deref()?)?.clone();
          let cont = fields.get(self.defs.cont_key.as_deref()?)?.clone();
          let input_success = self.to_reach(&input, depth + 1)?.success();
          let this = Value { objects: vec![Rc::clone(object)], ..Value::default() };
          let fiber = Value { fiber: true, ..Value::default() };
          let ret =
            self.call_value_def(&cont, &this, &[Value::reach(input_success), fiber], depth + 1)?;
          reach = reach.join(self.to_reach(&ret, depth + 1)?.success().effect_of());
        }
        Some(_) => return None,
        None => {
          for field in fields.values().chain(std::iter::once(&other)) {
            if !field.callables.is_empty() && !field.reach.any() {
              continue;
            }
            reach = reach.join(self.to_reach(field, depth + 1)?);
          }
        }
      }
    }
    Some(reach)
  }

  fn exec_def(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    statements: &'s ArenaVec<'a, Statement<'a>>,
    depth: usize,
  ) -> Option<Flow<'s, 'a>> {
    let mut flow = Flow { falls_through: true, ..Flow::default() };
    for statement in statements {
      if !flow.falls_through {
        break;
      }
      let next = self.exec_statement(ctx, statement, depth)?;
      flow.returns = std::mem::take(&mut flow.returns).join(next.returns);
      flow.falls_through = next.falls_through;
      flow.broke |= next.broke;
    }
    Some(flow)
  }

  fn exec_statement(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    statement: &'s Statement<'a>,
    depth: usize,
  ) -> Option<Flow<'s, 'a>> {
    let through = || Flow { falls_through: true, ..Flow::default() };
    Some(match statement {
      Statement::EmptyStatement(_) | Statement::DebuggerStatement(_) => through(),
      Statement::ExpressionStatement(expression) => {
        self.eval_def(ctx, &expression.expression, depth)?;
        through()
      }
      Statement::ReturnStatement(statement) => {
        let returns = match &statement.argument {
          Some(argument) => self.eval_def(ctx, argument, depth)?,
          None => Value::constant(Constant::Undefined),
        };
        Flow { returns, falls_through: false, broke: false }
      }
      Statement::ThrowStatement(statement) => {
        let value = self.eval_def(ctx, &statement.argument, depth)?;
        let thrown = self.thrown_reach(&value, depth);
        let mut changed = self.state.changed;
        ReachState::join_field(&mut self.state.thrown, thrown, &mut changed);
        self.state.changed = changed;
        Flow::default()
      }
      Statement::BreakStatement(statement) if statement.label.is_none() => {
        Flow { broke: true, ..Flow::default() }
      }
      Statement::VariableDeclaration(declaration) => {
        for declarator in &declaration.declarations {
          let BindingPattern::BindingIdentifier(id) = &declarator.id else { return None };
          let value = match &declarator.init {
            Some(init) => self.eval_def(ctx, init, depth)?,
            None => Value::constant(Constant::Undefined),
          };
          ctx.frame.declare((ctx.module, id.symbol_id.get()?), value);
        }
        through()
      }
      Statement::FunctionDeclaration(function) => {
        let body = function.body.as_deref()?;
        let id = function.id.as_ref()?.symbol_id.get()?;
        ctx.frame.declare(
          (ctx.module, id),
          Self::closure(ctx, &function.params, ClosureBody::Block(body), false),
        );
        through()
      }
      Statement::BlockStatement(block) => self.exec_def(ctx, &block.body, depth)?,
      Statement::IfStatement(statement) => match self.eval_test(ctx, &statement.test, depth) {
        Some(true) => self.exec_statement(ctx, &statement.consequent, depth)?,
        Some(false) => match &statement.alternate {
          Some(alternate) => self.exec_statement(ctx, alternate, depth)?,
          None => through(),
        },
        None => {
          let consequent = self.exec_statement(ctx, &statement.consequent, depth)?;
          let alternate = match &statement.alternate {
            Some(alternate) => self.exec_statement(ctx, alternate, depth)?,
            None => through(),
          };
          Flow {
            returns: consequent.returns.join(alternate.returns),
            falls_through: consequent.falls_through || alternate.falls_through,
            broke: consequent.broke || alternate.broke,
          }
        }
      },
      Statement::SwitchStatement(switch) => {
        let discriminant =
          self.eval_def(ctx, &switch.discriminant, depth).unwrap_or_else(Value::unknown);
        // The case to start from, when the discriminant and every test before
        // the match are constants.
        let mut start = None;
        if let Some(constant) = &discriminant.constant {
          for (index, case) in switch.cases.iter().enumerate() {
            let Some(test) = &case.test else { continue };
            let test = self.eval_def(ctx, test, depth).unwrap_or_else(Value::unknown);
            match &test.constant {
              Some(value) if value == constant => {
                start = Some(index);
                break;
              }
              Some(_) => {}
              None => break,
            }
          }
          if start.is_none() {
            start = switch.cases.iter().position(|case| case.test.is_none());
          }
        }
        let starts: Vec<usize> = match start {
          Some(start) => vec![start],
          None => (0..switch.cases.len()).collect(),
        };
        let mut flow = Flow::default();
        for start in starts {
          let mut run = Flow { falls_through: true, ..Flow::default() };
          for case in switch.cases.iter().skip(start) {
            let next = self.exec_def(ctx, &case.consequent, depth)?;
            run.returns = std::mem::take(&mut run.returns).join(next.returns);
            run.falls_through = next.falls_through;
            if next.broke {
              run.falls_through = true;
              break;
            }
            if !next.falls_through {
              break;
            }
          }
          flow.returns = std::mem::take(&mut flow.returns).join(run.returns);
          flow.falls_through |= run.falls_through;
        }
        flow
      }
      Statement::TryStatement(statement) => {
        let block = self.exec_def(ctx, &statement.block.body, depth)?;
        let mut flow = block;
        if let Some(handler) = &statement.handler {
          if let Some(param) = &handler.param {
            let BindingPattern::BindingIdentifier(id) = &param.pattern else { return None };
            ctx.frame.declare(
              (ctx.module, id.symbol_id.get()?),
              Value::reach(self.state.thrown.collapse()),
            );
          }
          let caught = self.exec_def(ctx, &handler.body.body, depth)?;
          flow.returns = flow.returns.join(caught.returns);
          flow.falls_through |= caught.falls_through;
        }
        if let Some(finalizer) = &statement.finalizer {
          let finalizer = self.exec_def(ctx, &finalizer.body, depth)?;
          flow.returns = flow.returns.join(finalizer.returns);
          if !finalizer.falls_through {
            flow.falls_through = false;
          }
        }
        flow
      }
      _ => return None,
    })
  }

  /// What a thrown value or failure cause may reach. A value Effect's code
  /// built without the program's values carries none of them.
  fn thrown_reach(&mut self, value: &Value<'s, 'a>, depth: usize) -> Reach {
    if !value.carries_program() {
      return Reach::NONE;
    }
    self.to_reach(value, depth).unwrap_or(Reach::TOP).collapse()
  }

  /// What the fiber does with a primitive built with `args`.
  fn primitive(
    &mut self,
    primitive: Primitive,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    let first = args.first().cloned().unwrap_or_default();
    let fiber = Value { fiber: true, ..Value::default() };
    Some(match primitive {
      // `exitSucceed(value)`: succeeds with `value`.
      Primitive::Success => Value::reach(self.to_reach(&first, depth + 1)?.effect_of()),
      // `exitFailCause(cause)`: fails; error handlers may receive the cause.
      Primitive::Failure => {
        let thrown = self.thrown_reach(&first, depth + 1);
        let mut changed = self.state.changed;
        ReachState::join_field(&mut self.state.thrown, thrown, &mut changed);
        self.state.changed = changed;
        Value::default()
      }
      // `withFiber(f)`: runs the Effect `f(fiber)` returns.
      Primitive::WithFiber => {
        let ret = self.call_value_def(&first, &Value::default(), &[fiber], depth + 1)?;
        Value::reach(self.to_reach(&ret, depth + 1)?.success().effect_of())
      }
      // `withFiberSucceed(f)`: succeeds with `f(fiber)`.
      Primitive::WithFiberSucceed => {
        let ret = self.call_value_def(&first, &Value::default(), &[fiber], depth + 1)?;
        Value::reach(self.to_reach(&ret, depth + 1)?.effect_of())
      }
      // `sync(f)`: succeeds with `f()`.
      Primitive::Sync => {
        let ret = self.call_value_def(&first, &Value::default(), &[], depth + 1)?;
        Value::reach(self.to_reach(&ret, depth + 1)?.effect_of())
      }
      // `suspend(f)`: runs the Effect `f()` returns.
      Primitive::Suspend => {
        let ret = self.call_value_def(&first, &Value::default(), &[], depth + 1)?;
        Value::reach(self.to_reach(&ret, depth + 1)?.success().effect_of())
      }
      // Built with `new` from its prototype, never called.
      Primitive::OnSuccess => return None,
    })
  }
}
