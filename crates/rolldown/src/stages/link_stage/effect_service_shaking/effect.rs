//! The Effect model: what the stable primitives of `effect` 4.x do with
//! services. This is the only file that names an Effect API.
//!
//! Rules bind to the canonical symbols of the public exports listed in
//! `MODULES` (see `mod.rs`), so `internalEffect.map` inside `effect`'s own
//! modules hits the same rule as `Effect.map` in user code. The modelled
//! modules are the public ones listed here and the internal modules their ruled
//! exports resolve into. Their exports without an explicit rule get
//! `effect:default`, which is sound by construction: callbacks receive every
//! argument-derived value plus the runtime objects Effect hands out, and the
//! result yields the same. Every other module, inside `effect` or not, is
//! analysed as ordinary code (design section 3).

#![expect(clippy::many_single_char_names, reason = "rules are small closures over `a` and `c`")]

use arcstr::ArcStr;

use super::heap::{Analysis, Arg, Call, Constraint, KeyUse, ObjId, ObjKind, Site, VarId};
use super::host::{
  arg, args_from, await_of, const_string_of, elem_of, host_var, invoke, invoke_args, load,
  new_array, new_object, new_promise, prim_var, this_of,
};

pub const SCOPE_KEY: &str = "effect/Scope";
pub const MEMO_MAP_KEY: &str = "effect/Layer/CurrentMemoMap";
pub const PARENT_SPAN_KEY: &str = "effect/Tracer/ParentSpan";
pub const FIBER_METRICS_KEY: &str = "effect/Metric/FiberRuntimeMetrics";

/// How a public export of a modelled module is bound.
#[derive(Clone, Copy, Debug)]
pub enum Spec {
  /// A rule (callable) by name.
  Rule(&'static str),
  /// An Effect value with no success.
  EffectValue,
  /// A fixed service key.
  Key(&'static str),
  /// An Effect whose success is the current Fiber.
  FiberEffect,
  /// An empty Layer.
  EmptyLayer,
  /// `Pipeable.Prototype`: an object whose `pipe` method applies its arguments in order.
  PipeablePrototype,
  /// `Pipeable.Class`: a constructor that does nothing, with `Pipeable.Prototype` as prototype.
  PipeableClass,
}

/// The `effect` major version the model is written against.
pub const SUPPORTED_MAJOR: &str = "4";

/// Public modules and their explicit exports. Every other export of these
/// modules gets `effect:default`.
pub const MODULES: &[(&str, &[(&str, Spec)])] = &[
  (
    "Context",
    &[
      ("Service", Spec::Rule("effect:Context.Service")),
      ("Reference", Spec::Rule("effect:Context.Reference")),
      ("make", Spec::Rule("effect:Context.make")),
      ("add", Spec::Rule("effect:Context.add")),
      ("get", Spec::Rule("effect:Context.get")),
      ("getUnsafe", Spec::Rule("effect:Context.get")),
      ("getOption", Spec::Rule("effect:Context.getOption")),
      ("getOrUndefined", Spec::Rule("effect:Context.getOption")),
      ("getOrElse", Spec::Rule("effect:Context.getOrElse")),
      ("getOrUndefinedUnsafe", Spec::Rule("effect:Context.getOrUndefinedUnsafe")),
      ("makeUnsafe", Spec::Rule("effect:Context.makeUnsafe")),
      ("merge", Spec::Rule("effect:Context.merge")),
      ("mergeAll", Spec::Rule("effect:Context.mergeAll")),
      ("pick", Spec::Rule("effect:Context.pick")),
      ("omit", Spec::Rule("effect:Context.omit")),
      ("empty", Spec::Rule("effect:Context.empty")),
      ("isContext", Spec::Rule("effect:prim")),
      ("isReference", Spec::Rule("effect:prim")),
    ],
  ),
  (
    "Effect",
    &[
      ("succeed", Spec::Rule("effect:Effect.succeed")),
      ("sync", Spec::Rule("effect:Effect.sync")),
      ("suspend", Spec::Rule("effect:Effect.suspend")),
      ("promise", Spec::Rule("effect:Effect.promise")),
      ("tryPromise", Spec::Rule("effect:Effect.tryPromise")),
      ("try", Spec::Rule("effect:Effect.try")),
      ("fail", Spec::Rule("effect:Effect.fail")),
      ("failSync", Spec::Rule("effect:Effect.failSync")),
      ("failCause", Spec::Rule("effect:Effect.fail")),
      ("failCauseSync", Spec::Rule("effect:Effect.failSync")),
      ("die", Spec::Rule("effect:Effect.fail")),
      ("callback", Spec::Rule("effect:Effect.callback")),
      ("void", Spec::EffectValue),
      ("never", Spec::EffectValue),
      ("interrupt", Spec::EffectValue),
      ("yieldNow", Spec::EffectValue),
      ("Do", Spec::EffectValue),
      ("succeedNone", Spec::EffectValue),
      ("txRetry", Spec::EffectValue),
      ("tracer", Spec::EffectValue),
      ("currentSpan", Spec::EffectValue),
      ("currentParentSpan", Spec::EffectValue),
      ("spanAnnotations", Spec::EffectValue),
      ("spanLinks", Spec::EffectValue),
      ("abortSignal", Spec::EffectValue),
      ("fiberId", Spec::EffectValue),
      ("fiber", Spec::FiberEffect),
      ("scope", Spec::Key(SCOPE_KEY)),
      ("map", Spec::Rule("effect:Effect.map")),
      ("mapEager", Spec::Rule("effect:Effect.map")),
      ("flatMap", Spec::Rule("effect:Effect.flatMap")),
      ("flatMapEager", Spec::Rule("effect:Effect.flatMap")),
      ("andThen", Spec::Rule("effect:Effect.andThen")),
      ("tap", Spec::Rule("effect:Effect.tap")),
      ("as", Spec::Rule("effect:Effect.as")),
      ("asVoid", Spec::Rule("effect:Effect.passthrough")),
      ("asSome", Spec::Rule("effect:Effect.wrapSuccess")),
      ("flatten", Spec::Rule("effect:Effect.flatten")),
      ("zip", Spec::Rule("effect:Effect.zip")),
      ("zipWith", Spec::Rule("effect:Effect.zipWith")),
      ("all", Spec::Rule("effect:Effect.all")),
      ("forEach", Spec::Rule("effect:Effect.forEach")),
      ("gen", Spec::Rule("effect:Effect.gen")),
      ("fn", Spec::Rule("effect:Effect.fn")),
      ("fnUntraced", Spec::Rule("effect:Effect.fn")),
      ("fnUntracedEager", Spec::Rule("effect:Effect.fn")),
      ("service", Spec::Rule("effect:identity")),
      ("serviceOption", Spec::Rule("effect:Effect.serviceOption")),
      ("context", Spec::Rule("effect:Effect.context")),
      ("contextWith", Spec::Rule("effect:Effect.contextWith")),
      ("withFiber", Spec::Rule("effect:Effect.withFiber")),
      ("withFiberSucceed", Spec::Rule("effect:Effect.withFiberSucceed")),
      ("provide", Spec::Rule("effect:Effect.provide")),
      ("provideContext", Spec::Rule("effect:Effect.provide")),
      ("setContext", Spec::Rule("effect:Effect.provide")),
      ("provideService", Spec::Rule("effect:Effect.provideService")),
      ("provideServiceEffect", Spec::Rule("effect:Effect.provideServiceEffect")),
      ("updateContext", Spec::Rule("effect:Effect.updateContext")),
      ("updateService", Spec::Rule("effect:Effect.updateService")),
      ("updateServiceScoped", Spec::Rule("effect:Effect.updateServiceScoped")),
      ("scoped", Spec::Rule("effect:Effect.passthrough")),
      ("scopedWith", Spec::Rule("effect:Effect.scopedWith")),
      ("acquireRelease", Spec::Rule("effect:Effect.acquireRelease")),
      ("acquireUseRelease", Spec::Rule("effect:Effect.acquireUseRelease")),
      ("addFinalizer", Spec::Rule("effect:Effect.addFinalizer")),
      ("catch", Spec::Rule("effect:Effect.catch")),
      ("catchEager", Spec::Rule("effect:Effect.catch")),
      ("catchCause", Spec::Rule("effect:Effect.catch")),
      ("catchDefect", Spec::Rule("effect:Effect.catch")),
      ("catchIf", Spec::Rule("effect:Effect.catchIf")),
      ("catchFilter", Spec::Rule("effect:Effect.catchIf")),
      ("catchCauseIf", Spec::Rule("effect:Effect.catchIf")),
      ("catchCauseFilter", Spec::Rule("effect:Effect.catchIf")),
      ("catchTag", Spec::Rule("effect:Effect.catchIf")),
      ("catchReason", Spec::Rule("effect:Effect.catchIf")),
      ("catchTags", Spec::Rule("effect:Effect.catchTags")),
      ("catchReasons", Spec::Rule("effect:Effect.catchTags")),
      ("orElseSucceed", Spec::Rule("effect:Effect.orElseSucceed")),
      ("match", Spec::Rule("effect:Effect.match")),
      ("matchEager", Spec::Rule("effect:Effect.match")),
      ("matchEffect", Spec::Rule("effect:Effect.matchEffect")),
      ("matchCause", Spec::Rule("effect:Effect.match")),
      ("matchCauseEager", Spec::Rule("effect:Effect.match")),
      ("matchCauseEffect", Spec::Rule("effect:Effect.matchEffect")),
      ("matchCauseEffectEager", Spec::Rule("effect:Effect.matchEffect")),
      ("mapError", Spec::Rule("effect:Effect.mapError")),
      ("mapErrorEager", Spec::Rule("effect:Effect.mapError")),
      ("mapBoth", Spec::Rule("effect:Effect.mapBoth")),
      ("mapBothEager", Spec::Rule("effect:Effect.mapBoth")),
      ("orDie", Spec::Rule("effect:Effect.passthrough")),
      ("ignore", Spec::Rule("effect:Effect.passthrough")),
      ("ignoreCause", Spec::Rule("effect:Effect.passthrough")),
      ("result", Spec::Rule("effect:Effect.wrapBoth")),
      ("option", Spec::Rule("effect:Effect.wrapBoth")),
      ("exit", Spec::Rule("effect:Effect.wrapBoth")),
      ("sandbox", Spec::Rule("effect:Effect.passthrough")),
      ("tapError", Spec::Rule("effect:Effect.tapError")),
      ("tapCause", Spec::Rule("effect:Effect.tapError")),
      ("tapDefect", Spec::Rule("effect:Effect.tapError")),
      ("tapErrorTag", Spec::Rule("effect:Effect.tapErrorTag")),
      ("tapCauseIf", Spec::Rule("effect:Effect.tapErrorTag")),
      ("tapCauseFilter", Spec::Rule("effect:Effect.tapErrorTag")),
      ("onError", Spec::Rule("effect:Effect.tapError")),
      ("onExit", Spec::Rule("effect:Effect.onExit")),
      ("onExitPrimitive", Spec::Rule("effect:Effect.onExit")),
      ("ensuring", Spec::Rule("effect:Effect.passthrough")),
      ("onInterrupt", Spec::Rule("effect:Effect.passthrough")),
      ("retry", Spec::Rule("effect:Effect.passthrough")),
      ("retryOrElse", Spec::Rule("effect:Effect.orElseSucceed")),
      ("repeat", Spec::Rule("effect:Effect.passthrough")),
      ("forever", Spec::Rule("effect:Effect.passthrough")),
      ("timeout", Spec::Rule("effect:Effect.passthrough")),
      ("timeoutOption", Spec::Rule("effect:Effect.wrapSuccess")),
      ("timeoutOrElse", Spec::Rule("effect:Effect.orElseSucceed")),
      ("delay", Spec::Rule("effect:Effect.passthrough")),
      ("timed", Spec::Rule("effect:Effect.wrapSuccess")),
      ("uninterruptible", Spec::Rule("effect:Effect.passthrough")),
      ("interruptible", Spec::Rule("effect:Effect.passthrough")),
      ("uninterruptibleMask", Spec::Rule("effect:Effect.mask")),
      ("interruptibleMask", Spec::Rule("effect:Effect.mask")),
      ("cached", Spec::Rule("effect:Effect.cached")),
      ("cachedWithTTL", Spec::Rule("effect:Effect.cached")),
      ("cachedInvalidateWithTTL", Spec::Rule("effect:Effect.cached")),
      ("withSpan", Spec::Rule("effect:Effect.passthrough")),
      ("withSpanScoped", Spec::Rule("effect:Effect.passthrough")),
      ("withParentSpan", Spec::Rule("effect:Effect.passthrough")),
      ("annotateLogs", Spec::Rule("effect:Effect.passthrough")),
      ("annotateSpans", Spec::Rule("effect:Effect.passthrough")),
      ("withLogSpan", Spec::Rule("effect:Effect.passthrough")),
      ("withTracer", Spec::Rule("effect:Effect.passthrough")),
      ("withTracerEnabled", Spec::Rule("effect:Effect.passthrough")),
      ("withTracerTiming", Spec::Rule("effect:Effect.passthrough")),
      ("withLogger", Spec::Rule("effect:Effect.passthrough")),
      ("linkSpans", Spec::Rule("effect:Effect.passthrough")),
      ("when", Spec::Rule("effect:Effect.wrapSuccess")),
      ("filterOrFail", Spec::Rule("effect:Effect.passthrough")),
      ("filterOrElse", Spec::Rule("effect:Effect.orElseSucceed")),
      ("forkChild", Spec::Rule("effect:Effect.fork")),
      ("forkIn", Spec::Rule("effect:Effect.fork")),
      ("forkScoped", Spec::Rule("effect:Effect.fork")),
      ("forkDetach", Spec::Rule("effect:Effect.fork")),
      ("awaitAllChildren", Spec::Rule("effect:Effect.passthrough")),
      ("runPromise", Spec::Rule("effect:Effect.runPromise")),
      ("runPromiseExit", Spec::Rule("effect:Effect.runPromiseExit")),
      ("runSync", Spec::Rule("effect:Effect.runSync")),
      ("runSyncExit", Spec::Rule("effect:Effect.runSyncExit")),
      ("runFork", Spec::Rule("effect:Effect.runFork")),
      ("runCallback", Spec::Rule("effect:Effect.runCallback")),
      ("bind", Spec::Rule("effect:Effect.bind")),
      ("bindTo", Spec::Rule("effect:Effect.bindTo")),
      ("let", Spec::Rule("effect:Effect.bind")),
      ("isEffect", Spec::Rule("effect:prim")),
      ("isFailure", Spec::Rule("effect:Effect.passthrough")),
      ("isSuccess", Spec::Rule("effect:Effect.passthrough")),
      ("satisfiesSuccessType", Spec::Rule("effect:constIdentity")),
      ("satisfiesErrorType", Spec::Rule("effect:constIdentity")),
      ("satisfiesServicesType", Spec::Rule("effect:constIdentity")),
      ("log", Spec::Rule("effect:Effect.log")),
      ("logFatal", Spec::Rule("effect:Effect.log")),
      ("logWarning", Spec::Rule("effect:Effect.log")),
      ("logError", Spec::Rule("effect:Effect.log")),
      ("logInfo", Spec::Rule("effect:Effect.log")),
      ("logDebug", Spec::Rule("effect:Effect.log")),
      ("logTrace", Spec::Rule("effect:Effect.log")),
    ],
  ),
  (
    "Layer",
    &[
      ("succeed", Spec::Rule("effect:Layer.succeed")),
      ("effect", Spec::Rule("effect:Layer.effect")),
      ("sync", Spec::Rule("effect:Layer.sync")),
      ("succeedContext", Spec::Rule("effect:Layer.succeedContext")),
      ("effectContext", Spec::Rule("effect:Layer.effectContext")),
      ("syncContext", Spec::Rule("effect:Layer.syncContext")),
      ("effectDiscard", Spec::Rule("effect:Layer.effectDiscard")),
      ("mock", Spec::Rule("effect:Layer.succeed")),
      ("merge", Spec::Rule("effect:Layer.merge")),
      ("mergeAll", Spec::Rule("effect:Layer.mergeAll")),
      ("provide", Spec::Rule("effect:Layer.provide")),
      ("provideMerge", Spec::Rule("effect:Layer.provideMerge")),
      ("updateService", Spec::Rule("effect:Layer.updateService")),
      ("fresh", Spec::Rule("effect:Layer.passthrough")),
      ("orDie", Spec::Rule("effect:Layer.passthrough")),
      ("catchTag", Spec::Rule("effect:Layer.catch")),
      ("catchCause", Spec::Rule("effect:Layer.catch")),
      ("tap", Spec::Rule("effect:Layer.tap")),
      ("tapError", Spec::Rule("effect:Layer.tap")),
      ("tapCause", Spec::Rule("effect:Layer.tap")),
      ("flatMap", Spec::Rule("effect:Layer.flatMap")),
      ("unwrap", Spec::Rule("effect:Layer.unwrap")),
      ("suspend", Spec::Rule("effect:Layer.suspend")),
      ("span", Spec::Rule("effect:Layer.span")),
      ("parentSpan", Spec::Rule("effect:Layer.span")),
      ("withSpan", Spec::Rule("effect:Layer.passthrough")),
      ("withParentSpan", Spec::Rule("effect:Layer.passthrough")),
      ("launch", Spec::Rule("effect:Layer.launch")),
      ("build", Spec::Rule("effect:Layer.build")),
      ("buildWithScope", Spec::Rule("effect:Layer.build")),
      ("buildWithMemoMap", Spec::Rule("effect:Layer.build")),
      ("fromBuild", Spec::Rule("effect:Layer.fromBuild")),
      ("fromBuildMemo", Spec::Rule("effect:Layer.fromBuild")),
      ("makeMemoMapUnsafe", Spec::Rule("effect:MemoMap.make")),
      ("forkMemoMapUnsafe", Spec::Rule("effect:MemoMap.make")),
      ("makeMemoMap", Spec::EffectValue),
      ("forkMemoMap", Spec::Rule("effect:Effect.void")),
      ("CurrentMemoMap", Spec::Key(MEMO_MAP_KEY)),
      ("empty", Spec::EmptyLayer),
      ("isLayer", Spec::Rule("effect:prim")),
      ("satisfiesSuccessType", Spec::Rule("effect:constIdentity")),
      ("satisfiesErrorType", Spec::Rule("effect:constIdentity")),
      ("satisfiesServicesType", Spec::Rule("effect:constIdentity")),
    ],
  ),
  (
    "ManagedRuntime",
    &[
      ("make", Spec::Rule("effect:ManagedRuntime.make")),
      ("isManagedRuntime", Spec::Rule("effect:prim")),
    ],
  ),
  ("Scope", &[("Scope", Spec::Key(SCOPE_KEY))]),
  (
    "Fiber",
    &[
      ("join", Spec::Rule("effect:Fiber.join")),
      ("await", Spec::Rule("effect:Fiber.await")),
      ("getCurrent", Spec::Rule("effect:Fiber.getCurrent")),
      ("isFiber", Spec::Rule("effect:prim")),
    ],
  ),
  (
    "Function",
    &[
      ("dual", Spec::Rule("effect:Function.dual")),
      ("pipe", Spec::Rule("effect:Function.pipe")),
      ("flow", Spec::Rule("effect:Function.flow")),
      ("compose", Spec::Rule("effect:Function.flow")),
      ("identity", Spec::Rule("effect:identity")),
      ("cast", Spec::Rule("effect:identity")),
      ("hole", Spec::Rule("effect:prim")),
      ("constant", Spec::Rule("effect:Function.constant")),
      ("constTrue", Spec::Rule("effect:prim")),
      ("constFalse", Spec::Rule("effect:prim")),
      ("constNull", Spec::Rule("effect:prim")),
      ("constUndefined", Spec::Rule("effect:prim")),
      ("constVoid", Spec::Rule("effect:prim")),
      ("apply", Spec::Rule("effect:Function.apply")),
      ("satisfies", Spec::Rule("effect:constIdentity")),
      ("memoize", Spec::Rule("effect:identity")),
      ("memoizeIdempotent", Spec::Rule("effect:identity")),
      ("SK", Spec::Rule("effect:Function.SK")),
    ],
  ),
  (
    "Pipeable",
    &[
      ("pipeArguments", Spec::Rule("effect:Pipeable.pipeArguments")),
      ("Prototype", Spec::PipeablePrototype),
      ("Class", Spec::PipeableClass),
    ],
  ),
  ("Tracer", &[("ParentSpan", Spec::Key(PARENT_SPAN_KEY))]),
];

/// Pure callables whose results are safe to drop with a removed property.
pub const PURE_RULES: &[&str] = &[
  "effect:Effect.fn",
  "effect:Function.dual",
  "effect:Function.flow",
  "effect:Function.constant",
  "effect:identity",
  "effect:constIdentity",
];

// == helpers

fn fiber(a: &mut Analysis) -> VarId {
  let f = a.site_obj(
    Site { module: rolldown_common::ModuleIdx::from_raw(0), start: 0, sub: 0 },
    250,
    |_| ObjKind::Fiber,
  );
  let v = a.var();
  a.seed(v, f);
  v
}

fn ambient(a: &mut Analysis) -> VarId {
  let v = a.var();
  a.seed(v, a.ambient_context);
  v
}

fn new_effect(a: &mut Analysis, site: Site, tag: u8) -> (VarId, VarId, VarId) {
  let (obj, success, failure) = a.new_effect(site, tag);
  let v = a.var();
  a.seed(v, obj);
  (v, success, failure)
}

fn new_layer(a: &mut Analysis, site: Site, tag: u8) -> (VarId, VarId) {
  let obj = a.site_obj(site, tag, |a| {
    let keys = a.var();
    ObjKind::Layer { keys }
  });
  let ObjKind::Layer { keys } = a.objs[obj].kind else { unreachable!() };
  let v = a.var();
  a.seed(v, obj);
  (v, keys)
}

fn new_context(a: &mut Analysis, site: Site, tag: u8) -> (VarId, VarId) {
  let obj = a.site_obj(site, tag, |a| {
    let keys = a.var();
    ObjKind::Context { keys }
  });
  let ObjKind::Context { keys } = a.objs[obj].kind else { unreachable!() };
  let v = a.var();
  a.seed(v, obj);
  (v, keys)
}

fn success_of(a: &mut Analysis, v: VarId) -> VarId {
  let dst = a.var();
  a.constrain(v, Constraint::SuccessOf(dst));
  dst
}

fn failure_of(a: &mut Analysis, v: VarId) -> VarId {
  let dst = a.var();
  a.constrain(v, Constraint::FailureOf(dst));
  dst
}

fn lookup(a: &mut Analysis, key: VarId) -> VarId {
  let dst = a.var();
  a.constrain(key, Constraint::KeyUse(KeyUse::Lookup(dst)));
  dst
}

fn provide(a: &mut Analysis, key: VarId, src: VarId) {
  a.constrain(key, Constraint::KeyUse(KeyUse::Provide(src)));
}

fn key_strings(a: &mut Analysis, key: VarId, dst: VarId) {
  a.constrain(key, Constraint::KeyUse(KeyUse::Strings(dst)));
}

fn context_keys(a: &mut Analysis, v: VarId, dst: VarId) {
  a.constrain(v, Constraint::ContextKeys(dst));
}

fn wrap(a: &mut Analysis, site: Site, inner: VarId) -> VarId {
  a.wrap_of(inner, site)
}

/// The effect `self` of a dual call: argument 0.
fn self_effect(a: &mut Analysis, c: &Call) -> VarId {
  arg(a, c, 0)
}

/// `Effect.X(self, ...)` with the data-first/data-last convention: when the call has fewer
/// arguments than `arity`, returns a function waiting for `self`.
fn partial(a: &mut Analysis, c: &Call, rule: &'static str) {
  let (obj, data) = new_object(a, c.site, 100);
  for (i, x) in c.args.iter().enumerate() {
    if let Arg::Var(v) = x {
      let f = a.field(obj, &ArcStr::from(format!("a{i}")));
      a.copy(*v, f);
    }
  }
  let target = a.rule_id(rule).unwrap_or_else(|| panic!("unknown rule {rule}"));
  let id = a.rule_id("effect:partial").unwrap();
  let b =
    a.builtin_full(id, Some(data), Some(u32::try_from(c.args.len()).unwrap_or(0)), Some(target));
  a.seed(c.ret, b);
}

macro_rules! dual {
  ($a:ident, $name:literal, $arity:literal, |$aa:ident, $cc:ident| $body:block) => {
    $a.rule($name, |$aa, $cc| {
      if $cc.args.len() >= $arity || $cc.has_spread() {
        $body
      } else {
        partial($aa, $cc, $name);
      }
    });
  };
}

fn effect_value(a: &mut Analysis, c: &Call) -> VarId {
  let (v, _, _) = new_effect(a, c.site, 1);
  v
}

/// Arguments after `self`, calling each function among them with the given values.
fn call_callbacks(a: &mut Analysis, c: &Call, from: usize, args: &[VarId]) -> VarId {
  let results = a.var();
  for x in args_from(a, c, from) {
    let v = match x {
      Arg::Var(v) | Arg::Spread(v) => v,
    };
    let r = invoke(a, c.site, v, None, args.to_vec());
    a.copy(r, results);
    // The argument may itself be an effect or value that the combinator uses.
    let s = success_of(a, v);
    a.copy(s, results);
  }
  results
}

#[expect(clippy::too_many_lines, reason = "one registration function for the model")]
pub fn register(a: &mut Analysis) {
  for k in [SCOPE_KEY, MEMO_MAP_KEY, PARENT_SPAN_KEY, FIBER_METRICS_KEY] {
    a.never_prune.insert(ArcStr::from(k));
  }
  // The runtime provides these keys itself: a memo map, and host data for the others.
  let memo = memo_map(a);
  let v = a.key_value_var(&ArcStr::from(MEMO_MAP_KEY));
  a.copy(memo, v);
  for k in [SCOPE_KEY, PARENT_SPAN_KEY, FIBER_METRICS_KEY] {
    let v = a.key_value_var(&ArcStr::from(k));
    a.seed(v, a.top_host);
  }

  a.rule("effect:prim", |a, c| a.seed(c.ret, a.prim));
  a.rule("effect:identity", |a, c| {
    let v = arg(a, c, 0);
    a.copy(v, c.ret);
  });
  a.rule("effect:constIdentity", |a, c| {
    let id = a.rule_id("effect:identity").unwrap();
    let b = a.builtin(id, None, None);
    a.seed(c.ret, b);
  });
  a.rule("effect:Effect.void", |a, c| {
    effect_value(a, c);
    let (v, _, _) = new_effect(a, c.site, 1);
    a.copy(v, c.ret);
  });
  a.rule("effect:partial", |a, c| {
    let data = this_of(a, c);
    let Some(id) = a.builtin_target(c) else { return };
    let n = a.builtin_arity(c).unwrap_or(0);
    let mut args: Vec<Arg> = vec![];
    // `self` first, then the captured arguments in order.
    args.push(Arg::Var(arg(a, c, 0)));
    for i in 0..n {
      let f = load(a, data, &format!("a{i}"));
      args.push(Arg::Var(f));
    }
    // Extra arguments of the partial call (rare) follow.
    args.extend(args_from(a, c, 1));
    let call = Call { site: c.site, this: None, args, ret: c.ret, is_new: false, builtin: None };
    (a.rules[id as usize].1)(a, &call);
  });

  // == Context

  a.rule("effect:Context.Service", |a, c| {
    if c.args.is_empty() {
      // `Context.Service<Self, Shape>()` returns the constructor.
      let id = a.rule_id("effect:Context.Service").unwrap();
      let b = a.builtin(id, None, None);
      a.seed(c.ret, b);
      return;
    }
    let strings = arg(a, c, 0);
    let key = a.site_obj(c.site, 2, |_| ObjKind::Key { strings, reference: false });
    if c.args.len() > 1 {
      let opts = arg(a, c, 1);
      let make = load(a, opts, "make");
      let mf = a.field(key, &ArcStr::from("make"));
      a.copy(make, mf);
    }
    a.seed(c.ret, key);
  });
  a.rule("effect:Context.Reference", |a, c| {
    let strings = arg(a, c, 0);
    let key = a.site_obj(c.site, 2, |_| ObjKind::Key { strings, reference: true });
    let kv = a.var();
    a.seed(kv, key);
    if c.args.len() > 1 {
      let opts = arg(a, c, 1);
      let default = load(a, opts, "defaultValue");
      let value = invoke(a, c.site, default, None, vec![]);
      provide(a, kv, value);
    }
    a.seed(c.ret, key);
  });
  a.rule("effect:Context.make", |a, c| {
    let key = arg(a, c, 0);
    let value = arg(a, c, 1);
    let (ctx, keys) = new_context(a, c.site, 3);
    key_strings(a, key, keys);
    provide(a, key, value);
    a.copy(ctx, c.ret);
  });
  dual!(a, "effect:Context.add", 3, |a, c| {
    let base = arg(a, c, 0);
    let key = arg(a, c, 1);
    let value = arg(a, c, 2);
    let (ctx, keys) = new_context(a, c.site, 3);
    context_keys(a, base, keys);
    key_strings(a, key, keys);
    provide(a, key, value);
    a.copy(ctx, c.ret);
  });
  dual!(a, "effect:Context.get", 2, |a, c| {
    let key = arg(a, c, 1);
    let v = lookup(a, key);
    a.copy(v, c.ret);
  });
  dual!(a, "effect:Context.getOption", 2, |a, c| {
    let key = arg(a, c, 1);
    let v = lookup(a, key);
    let w = wrap(a, c.site, v);
    a.copy(w, c.ret);
  });
  dual!(a, "effect:Context.getOrElse", 3, |a, c| {
    let key = arg(a, c, 1);
    let v = lookup(a, key);
    a.copy(v, c.ret);
    let f = arg(a, c, 2);
    let r = invoke(a, c.site, f, None, vec![]);
    a.copy(r, c.ret);
  });
  a.rule("effect:Context.getOrUndefinedUnsafe", |a, c| {
    let key = arg(a, c, 1);
    let v = lookup(a, key);
    a.copy(v, c.ret);
  });
  a.rule("effect:Context.makeUnsafe", |a, c| {
    let map = arg(a, c, 0);
    let (ctx, keys) = new_context(a, c.site, 3);
    let entries = elem_of(a, map);
    let names = a.var();
    a.constrain(entries, Constraint::MapKeys(names));
    a.copy(names, keys);
    provide(a, names, entries);
    a.copy(ctx, c.ret);
  });
  dual!(a, "effect:Context.merge", 2, |a, c| {
    let (ctx, keys) = new_context(a, c.site, 3);
    for x in args_from(a, c, 0) {
      let (Arg::Var(v) | Arg::Spread(v)) = x;
      context_keys(a, v, keys);
    }
    a.copy(ctx, c.ret);
  });
  a.rule("effect:Context.mergeAll", |a, c| {
    let (ctx, keys) = new_context(a, c.site, 3);
    for x in args_from(a, c, 0) {
      match x {
        Arg::Var(v) => context_keys(a, v, keys),
        Arg::Spread(v) => {
          let e = elem_of(a, v);
          context_keys(a, e, keys);
        }
      }
    }
    a.copy(ctx, c.ret);
  });
  a.rule("effect:Context.pick", |a, c| {
    // `pick(...keys)` returns a function of the Context.
    let (obj, data) = new_object(a, c.site, 4);
    let ks = a.field(obj, &ArcStr::from("keys"));
    for x in args_from(a, c, 0) {
      let (Arg::Var(v) | Arg::Spread(v)) = x;
      key_strings(a, v, ks);
    }
    let id = a.rule_id("effect:Context.pick.apply").unwrap();
    let b = a.builtin(id, Some(data), None);
    a.seed(c.ret, b);
  });
  a.rule("effect:Context.pick.apply", |a, c| {
    let data = this_of(a, c);
    let ks = load(a, data, "keys");
    let (ctx, keys) = new_context(a, c.site, 3);
    a.copy(ks, keys);
    a.copy(ctx, c.ret);
  });
  a.rule("effect:Context.omit", |a, c| {
    let id = a.rule_id("effect:Context.omit.apply").unwrap();
    let b = a.builtin(id, None, None);
    a.seed(c.ret, b);
  });
  a.rule("effect:Context.omit.apply", |a, c| {
    let base = arg(a, c, 0);
    let (ctx, keys) = new_context(a, c.site, 3);
    context_keys(a, base, keys);
    a.copy(ctx, c.ret);
  });
  a.rule("effect:Context.empty", |a, c| {
    let (ctx, _) = new_context(a, c.site, 3);
    a.copy(ctx, c.ret);
  });
  a.rule("Context.mapUnsafe:get", |a, c| {
    // The entries of the Context as a read-only Map: the values of its keys and the key strings.
    // Reading a name on a value demands that name of its key, like any other lookup; writing to
    // the Map or handing it to external code bails the keys (`view_of`).
    let this = this_of(a, c);
    let (map, arr, elem) = new_array(a, c.site, 5);
    // One view object per site: every Context read through it adds its keys to the view's.
    let keys = match a.objs[map].view_of {
      Some(keys) => keys,
      None => {
        let keys = a.var();
        a.objs[map].view_of = Some(keys);
        keys
      }
    };
    context_keys(a, this, keys);
    let values = lookup(a, keys);
    a.copy(values, elem);
    a.seed(elem, a.top_host);
    a.copy(arr, c.ret);
  });

  // == Key methods

  a.rule("Key.of", |a, c| {
    let v = arg(a, c, 0);
    a.copy(v, c.ret);
  });
  a.rule("Key.context", |a, c| {
    let key = this_of(a, c);
    let value = arg(a, c, 0);
    let (ctx, keys) = new_context(a, c.site, 3);
    key_strings(a, key, keys);
    provide(a, key, value);
    a.copy(ctx, c.ret);
  });
  a.rule("Key.use", |a, c| {
    let key = this_of(a, c);
    let f = arg(a, c, 0);
    let service = lookup(a, key);
    let r = invoke(a, c.site, f, None, vec![service]);
    let (e, s, fl) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, s);
    let rf = failure_of(a, r);
    a.copy(rf, fl);
    a.copy(e, c.ret);
  });
  a.rule("Key.useSync", |a, c| {
    let key = this_of(a, c);
    let f = arg(a, c, 0);
    let service = lookup(a, key);
    let r = invoke(a, c.site, f, None, vec![service]);
    let (e, s, _) = new_effect(a, c.site, 1);
    a.copy(r, s);
    a.copy(e, c.ret);
  });
  a.rule("Key.key:get", |a, c| {
    let key = this_of(a, c);
    key_strings(a, key, c.ret);
  });
  for name in [
    "Key.toJSON",
    "Key.toString",
    "Effect.toJSON",
    "Effect.toString",
    "Context.toJSON",
    "Context.toString",
    "Layer.toJSON",
    "Layer.toString",
  ] {
    a.rule(name, |a, c| a.seed(c.ret, a.top_host));
  }
  for name in [
    "Key.pipe",
    "Effect.pipe",
    "Layer.pipe",
    "Context.pipe",
    "Fiber.pipe",
    "Runtime.pipe",
    "MemoMap.pipe",
    "Promise.pipe",
    "Iterator.pipe",
    "effect:pipe.method",
  ] {
    a.rule(name, |a, c| {
      let mut acc = this_of(a, c);
      for x in args_from(a, c, 0) {
        match x {
          Arg::Var(f) => acc = invoke(a, c.site, f, None, vec![acc]),
          Arg::Spread(fs) => {
            let e = elem_of(a, fs);
            let next = a.var();
            a.copy(acc, next);
            let r = invoke(a, c.site, e, None, vec![next]);
            a.copy(r, next);
            acc = next;
          }
        }
      }
      a.copy(acc, c.ret);
    });
  }

  // == Effect constructors

  a.rule("effect:Effect.succeed", |a, c| {
    let v = arg(a, c, 0);
    let (e, s, _) = new_effect(a, c.site, 1);
    a.copy(v, s);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.sync", |a, c| {
    let f = arg(a, c, 0);
    let r = invoke(a, c.site, f, None, vec![]);
    let (e, s, _) = new_effect(a, c.site, 1);
    a.copy(r, s);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.suspend", |a, c| {
    let f = arg(a, c, 0);
    let r = invoke(a, c.site, f, None, vec![]);
    let (e, s, fl) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, s);
    let rf = failure_of(a, r);
    a.copy(rf, fl);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.promise", |a, c| {
    let f = arg(a, c, 0);
    let h = host_var(a);
    let r = invoke(a, c.site, f, None, vec![h]);
    let (e, s, _) = new_effect(a, c.site, 1);
    let settled = await_of(a, r);
    a.copy(settled, s);
    a.copy(e, c.ret);
  });
  for name in ["effect:Effect.tryPromise", "effect:Effect.try"] {
    a.rule(name, |a, c| {
      let f = arg(a, c, 0);
      let h = host_var(a);
      let (e, s, fl) = new_effect(a, c.site, 1);
      let r = invoke(a, c.site, f, None, vec![h]);
      let settled = await_of(a, r);
      a.copy(settled, s);
      let tryf = load(a, f, "try");
      let r2 = invoke(a, c.site, tryf, None, vec![h]);
      let settled2 = await_of(a, r2);
      a.copy(settled2, s);
      let catchf = load(a, f, "catch");
      let err = invoke(a, c.site, catchf, None, vec![h]);
      a.copy(err, fl);
      a.seed(fl, a.top_host);
      a.copy(e, c.ret);
    });
  }
  a.rule("effect:Effect.fail", |a, c| {
    let v = arg(a, c, 0);
    let (e, _, fl) = new_effect(a, c.site, 1);
    a.copy(v, fl);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.failSync", |a, c| {
    let f = arg(a, c, 0);
    let r = invoke(a, c.site, f, None, vec![]);
    let (e, _, fl) = new_effect(a, c.site, 1);
    a.copy(r, fl);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.callback", |a, c| {
    let f = arg(a, c, 0);
    let (e, s, fl) = new_effect(a, c.site, 1);
    let (obj, data) = new_object(a, c.site, 6);
    let sf = a.field(obj, &ArcStr::from("s"));
    a.copy(s, sf);
    let ff = a.field(obj, &ArcStr::from("f"));
    a.copy(fl, ff);
    let id = a.rule_id("effect:resume").unwrap();
    let resume = a.builtin(id, Some(data), None);
    let rv = a.var();
    a.seed(rv, resume);
    let h = host_var(a);
    let r = invoke(a, c.site, f, None, vec![rv, h]);
    // The callback may return a cleanup effect; it runs.
    let _ = r;
    a.copy(e, c.ret);
  });
  a.rule("effect:resume", |a, c| {
    let data = this_of(a, c);
    let eff = arg(a, c, 0);
    let s = success_of(a, eff);
    let sv = load(a, data, "s");
    // `s` is a variable holding the effect's success variable's contents: wire through.
    a.copy(s, sv);
    let fl = failure_of(a, eff);
    let fv = load(a, data, "f");
    a.copy(fl, fv);
  });

  // == Effect combinators

  dual!(a, "effect:Effect.map", 2, |a, c| {
    let e = self_effect(a, c);
    let f = arg(a, c, 1);
    let s = success_of(a, e);
    let r = invoke(a, c.site, f, None, vec![s]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(r, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.flatMap", 2, |a, c| {
    let e = self_effect(a, c);
    let f = arg(a, c, 1);
    let s = success_of(a, e);
    let r = invoke(a, c.site, f, None, vec![s]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.andThen", 2, |a, c| {
    let e = self_effect(a, c);
    let x = arg(a, c, 1);
    let s = success_of(a, e);
    let r = invoke(a, c.site, x, None, vec![s]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, os);
    let xs = success_of(a, x);
    a.copy(xs, os);
    // A non-effect result or argument is the value itself.
    a.copy(r, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    let xf = failure_of(a, x);
    a.copy(xf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.tap", 2, |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let r = call_callbacks(a, c, 1, &[s]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(s, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.as", 2, |a, c| {
    let e = self_effect(a, c);
    let v = arg(a, c, 1);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(v, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.passthrough", |a, c| {
    // Success and failure pass through; callbacks see everything the effect carries.
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    let r = call_callbacks(a, c, 1, &[w, w, w]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(s, os);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.wrapSuccess", |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let w = wrap(a, c.site, s);
    call_callbacks(a, c, 1, &[w]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(w, os);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.wrapBoth", |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    let (out, os, _) = new_effect(a, c.site, 1);
    a.copy(w, os);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.flatten", |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let (out, os, of) = new_effect(a, c.site, 1);
    let ss = success_of(a, s);
    a.copy(ss, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    let sf = failure_of(a, s);
    a.copy(sf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.zip", 2, |a, c| {
    let x = arg(a, c, 0);
    let y = arg(a, c, 1);
    let (out, os, of) = new_effect(a, c.site, 1);
    let (_, arr, elem) = new_array(a, c.site, 7);
    let xs = success_of(a, x);
    a.copy(xs, elem);
    let ys = success_of(a, y);
    a.copy(ys, elem);
    a.copy(arr, os);
    let xf = failure_of(a, x);
    a.copy(xf, of);
    let yf = failure_of(a, y);
    a.copy(yf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.zipWith", 3, |a, c| {
    let x = arg(a, c, 0);
    let y = arg(a, c, 1);
    let f = arg(a, c, 2);
    let xs = success_of(a, x);
    let ys = success_of(a, y);
    let r = invoke(a, c.site, f, None, vec![xs, ys]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(r, os);
    let xf = failure_of(a, x);
    a.copy(xf, of);
    let yf = failure_of(a, y);
    a.copy(yf, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.all", |a, c| {
    let items = arg(a, c, 0);
    let members = elem_of(a, items);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, members);
    let w = wrap(a, c.site, s);
    a.copy(w, os);
    let fl = failure_of(a, members);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.forEach", 2, |a, c| {
    let items = arg(a, c, 0);
    let f = arg(a, c, 1);
    let members = elem_of(a, items);
    let p = prim_var(a);
    let r = invoke(a, c.site, f, None, vec![members, p]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let (_, arr, elem) = new_array(a, c.site, 7);
    let rs = success_of(a, r);
    a.copy(rs, elem);
    a.copy(arr, os);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.gen", |a, c| {
    let (body, this) = if c.args.len() >= 2 {
      let opts = arg(a, c, 0);
      let self_ = load(a, opts, "self");
      (arg(a, c, 1), Some(self_))
    } else {
      (arg(a, c, 0), None)
    };
    let r = invoke(a, c.site, body, this, vec![]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, os);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.fn", |a, c| {
    // `Effect.fn("name", options)(body, ...pipeables)` or `Effect.fn(body, ...pipeables)`.
    let first = arg(a, c, 0);
    if const_string_of(a, first).is_some() || c.args.is_empty() {
      let id = a.rule_id("effect:Effect.fn").unwrap();
      let b = a.builtin(id, None, None);
      a.seed(c.ret, b);
      return;
    }
    let (obj, data) = new_object(a, c.site, 8);
    let body = a.field(obj, &ArcStr::from("body"));
    let this = a.field(obj, &ArcStr::from("this"));
    // `{ self }` form: the body is the second argument, bound to `self`.
    let self_ = load(a, first, "self");
    a.copy(self_, this);
    a.copy(first, body);
    let mut pipeables_from = 1;
    if c.args.len() >= 2 {
      let second = arg(a, c, 1);
      a.copy(second, body);
      pipeables_from = 1;
    }
    let (_, pipeables, pelem) = new_array(a, c.site, 9);
    for x in args_from(a, c, pipeables_from) {
      match x {
        Arg::Var(v) => a.copy(v, pelem),
        Arg::Spread(v) => a.constrain(v, Constraint::ElemOf(pelem)),
      }
    }
    let pf = a.field(obj, &ArcStr::from("pipeables"));
    a.copy(pipeables, pf);
    let id = a.rule_id("effect:Effect.fn.wrapper").unwrap();
    let b = a.builtin(id, Some(data), None);
    a.seed(c.ret, b);
  });
  a.rule("effect:Effect.fn.wrapper", |a, c| {
    let data = this_of(a, c);
    let body = load(a, data, "body");
    let this = load(a, data, "this");
    let args = args_from(a, c, 0);
    let r = invoke_args(a, c.site, body, Some(this), args.clone());
    let (e0, s, fl) = new_effect(a, c.site, 10);
    let rs = success_of(a, r);
    a.copy(rs, s);
    let rf = failure_of(a, r);
    a.copy(rf, fl);
    // Pipeables `(effect, ...args) => effect`, applied in an order the array model does not keep.
    let pipeables = load(a, data, "pipeables");
    let fs = elem_of(a, pipeables);
    let acc = a.var();
    a.copy(e0, acc);
    let mut pargs = vec![Arg::Var(acc)];
    pargs.extend(args);
    let r2 = invoke_args(a, c.site, fs, None, pargs);
    a.copy(r2, acc);
    a.copy(acc, c.ret);
  });
  a.rule("effect:Effect.serviceOption", |a, c| {
    let key = arg(a, c, 0);
    let v = lookup(a, key);
    let w = wrap(a, c.site, v);
    let (e, s, _) = new_effect(a, c.site, 1);
    a.copy(w, s);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.context", |a, c| {
    let ctx = ambient(a);
    let (e, s, _) = new_effect(a, c.site, 1);
    a.copy(ctx, s);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.contextWith", |a, c| {
    let f = arg(a, c, 0);
    let ctx = ambient(a);
    let r = invoke(a, c.site, f, None, vec![ctx]);
    let (e, s, fl) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, s);
    a.copy(r, s);
    let rf = failure_of(a, r);
    a.copy(rf, fl);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.withFiber", |a, c| {
    let f = arg(a, c, 0);
    let fb = fiber(a);
    let r = invoke(a, c.site, f, None, vec![fb]);
    let (e, s, fl) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, s);
    let rf = failure_of(a, r);
    a.copy(rf, fl);
    a.copy(e, c.ret);
  });
  a.rule("effect:Effect.withFiberSucceed", |a, c| {
    let f = arg(a, c, 0);
    let fb = fiber(a);
    let r = invoke(a, c.site, f, None, vec![fb]);
    let (e, s, _) = new_effect(a, c.site, 1);
    a.copy(r, s);
    a.copy(e, c.ret);
  });
  a.rule("Fiber.context:get", |a, c| {
    let ctx = ambient(a);
    a.copy(ctx, c.ret);
  });
  a.rule("Fiber.getRef", |a, c| {
    let key = arg(a, c, 0);
    let v = lookup(a, key);
    a.copy(v, c.ret);
  });
  a.rule("Fiber.setContext", |a, c| {
    let ctx = arg(a, c, 0);
    a.constrain(ctx, Constraint::Provision);
  });
  a.rule("Fiber.join", |a, c| {
    let fb = this_of(a, c);
    let v = load(a, fb, "~success");
    a.copy(v, c.ret);
  });
  dual!(a, "effect:Effect.provide", 2, |a, c| {
    let e = self_effect(a, c);
    let source = arg(a, c, 1);
    a.constrain(source, Constraint::Provision);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.provideService", |a, c| {
    match c.args.len() {
      0 => {}
      1 => {
        // `provideService(key)` returns `(self, impl)` / `(impl) => (self) => ...`.
        partial(a, c, "effect:Effect.provideService.keyed");
      }
      2 => {
        // `provideService(key, impl)` returns `(self) => ...`; provision happens now.
        let key = arg(a, c, 0);
        let impl_ = arg(a, c, 1);
        provide(a, key, impl_);
        let id = a.rule_id("effect:Effect.passthrough").unwrap();
        let b = a.builtin(id, None, None);
        a.seed(c.ret, b);
      }
      _ => {
        let e = self_effect(a, c);
        let key = arg(a, c, 1);
        let impl_ = arg(a, c, 2);
        provide(a, key, impl_);
        let (out, os, of) = new_effect(a, c.site, 1);
        let s = success_of(a, e);
        a.copy(s, os);
        let fl = failure_of(a, e);
        a.copy(fl, of);
        a.copy(out, c.ret);
      }
    }
  });
  a.rule("effect:Effect.provideService.keyed", |a, c| {
    // Reached through `partial`: args are [self_or_impl, key, ...].
    let key = arg(a, c, 1);
    if c.args.len() >= 3 {
      let e = arg(a, c, 0);
      let impl_ = arg(a, c, 2);
      provide(a, key, impl_);
      let (out, os, of) = new_effect(a, c.site, 1);
      let s = success_of(a, e);
      a.copy(s, os);
      let fl = failure_of(a, e);
      a.copy(fl, of);
      a.copy(out, c.ret);
    } else {
      let impl_ = arg(a, c, 0);
      provide(a, key, impl_);
      let id = a.rule_id("effect:Effect.passthrough").unwrap();
      let b = a.builtin(id, None, None);
      a.seed(c.ret, b);
    }
  });
  dual!(a, "effect:Effect.provideServiceEffect", 3, |a, c| {
    let e = self_effect(a, c);
    let key = arg(a, c, 1);
    let acquire = arg(a, c, 2);
    let value = success_of(a, acquire);
    provide(a, key, value);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    let af = failure_of(a, acquire);
    a.copy(af, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.updateContext", 2, |a, c| {
    let e = self_effect(a, c);
    let f = arg(a, c, 1);
    let ctx = ambient(a);
    let r = invoke(a, c.site, f, None, vec![ctx]);
    a.constrain(r, Constraint::Provision);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.updateService", 3, |a, c| {
    let e = self_effect(a, c);
    let key = arg(a, c, 1);
    let f = arg(a, c, 2);
    let current = lookup(a, key);
    let r = invoke(a, c.site, f, None, vec![current]);
    provide(a, key, r);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.updateServiceScoped", |a, c| {
    let key = arg(a, c, 0);
    let f = arg(a, c, 1);
    let current = lookup(a, key);
    let r = invoke(a, c.site, f, None, vec![current]);
    provide(a, key, r);
    if c.args.len() > 2 {
      let opts = arg(a, c, 2);
      let reset = load(a, opts, "reset");
      let r2 = invoke(a, c.site, reset, None, vec![current, r, current]);
      provide(a, key, r2);
    }
    let (out, _, _) = new_effect(a, c.site, 1);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.scopedWith", |a, c| {
    let f = arg(a, c, 0);
    let scope = host_var(a);
    let r = invoke(a, c.site, f, None, vec![scope]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, os);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.acquireRelease", |a, c| {
    let acquire = arg(a, c, 0);
    let release = arg(a, c, 1);
    let s = success_of(a, acquire);
    let exit = host_var(a);
    invoke(a, c.site, release, None, vec![s, exit]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(s, os);
    let fl = failure_of(a, acquire);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.acquireUseRelease", |a, c| {
    let acquire = arg(a, c, 0);
    let use_ = arg(a, c, 1);
    let release = arg(a, c, 2);
    let s = success_of(a, acquire);
    let exit = host_var(a);
    let used = invoke(a, c.site, use_, None, vec![s]);
    invoke(a, c.site, release, None, vec![s, exit]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let us = success_of(a, used);
    a.copy(us, os);
    let fl = failure_of(a, acquire);
    a.copy(fl, of);
    let uf = failure_of(a, used);
    a.copy(uf, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.addFinalizer", |a, c| {
    let f = arg(a, c, 0);
    let exit = host_var(a);
    invoke(a, c.site, f, None, vec![exit]);
    let (out, _, _) = new_effect(a, c.site, 1);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.catch", 2, |a, c| {
    let e = self_effect(a, c);
    let fl = failure_of(a, e);
    let r = call_callbacks(a, c, 1, &[fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    let rs = success_of(a, r);
    a.copy(rs, os);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.catchIf", 3, |a, c| {
    let e = self_effect(a, c);
    let fl = failure_of(a, e);
    let r = call_callbacks(a, c, 1, &[fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    let rs = success_of(a, r);
    a.copy(rs, os);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.catchTags", 2, |a, c| {
    let e = self_effect(a, c);
    let handlers = arg(a, c, 1);
    let fl = failure_of(a, e);
    let hs = elem_of(a, handlers);
    let r = invoke(a, c.site, hs, None, vec![fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    let rs = success_of(a, r);
    a.copy(rs, os);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.orElseSucceed", 2, |a, c| {
    let e = self_effect(a, c);
    let fl = failure_of(a, e);
    let r = call_callbacks(a, c, 1, &[fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    a.copy(r, os);
    let rs = success_of(a, r);
    a.copy(rs, os);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.match", 2, |a, c| {
    let e = self_effect(a, c);
    let opts = arg(a, c, 1);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let on_success = load(a, opts, "onSuccess");
    let on_failure = load(a, opts, "onFailure");
    let r1 = invoke(a, c.site, on_success, None, vec![s]);
    let r2 = invoke(a, c.site, on_failure, None, vec![fl]);
    let (out, os, _) = new_effect(a, c.site, 1);
    a.copy(r1, os);
    a.copy(r2, os);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.matchEffect", 2, |a, c| {
    let e = self_effect(a, c);
    let opts = arg(a, c, 1);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let on_success = load(a, opts, "onSuccess");
    let on_failure = load(a, opts, "onFailure");
    let r1 = invoke(a, c.site, on_success, None, vec![s]);
    let r2 = invoke(a, c.site, on_failure, None, vec![fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    for r in [r1, r2] {
      let rs = success_of(a, r);
      a.copy(rs, os);
      let rf = failure_of(a, r);
      a.copy(rf, of);
    }
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.mapError", 2, |a, c| {
    let e = self_effect(a, c);
    let f = arg(a, c, 1);
    let fl = failure_of(a, e);
    let r = invoke(a, c.site, f, None, vec![fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    a.copy(r, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.mapBoth", 2, |a, c| {
    let e = self_effect(a, c);
    let opts = arg(a, c, 1);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let on_success = load(a, opts, "onSuccess");
    let on_failure = load(a, opts, "onFailure");
    let r1 = invoke(a, c.site, on_success, None, vec![s]);
    let r2 = invoke(a, c.site, on_failure, None, vec![fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(r1, os);
    a.copy(r2, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.tapError", 2, |a, c| {
    let e = self_effect(a, c);
    let fl = failure_of(a, e);
    let r = call_callbacks(a, c, 1, &[fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.tapErrorTag", 3, |a, c| {
    let e = self_effect(a, c);
    let fl = failure_of(a, e);
    let r = call_callbacks(a, c, 1, &[fl]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let s = success_of(a, e);
    a.copy(s, os);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.onExit", 2, |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    call_callbacks(a, c, 1, &[w]);
    let (out, os, of) = new_effect(a, c.site, 1);
    a.copy(s, os);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.mask", |a, c| {
    let f = arg(a, c, 0);
    let id = a.rule_id("effect:identity").unwrap();
    let restore = a.builtin(id, None, None);
    let rv = a.var();
    a.seed(rv, restore);
    let r = invoke(a, c.site, f, None, vec![rv]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let rs = success_of(a, r);
    a.copy(rs, os);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.cached", |a, c| {
    let e = self_effect(a, c);
    let (out, os, _) = new_effect(a, c.site, 1);
    a.copy(e, os);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.fork", |a, c| {
    let e = self_effect(a, c);
    let fb = a.site_obj(c.site, 11, |_| ObjKind::Fiber);
    let s = success_of(a, e);
    let sf = a.field(fb, &ArcStr::from("~success"));
    a.copy(s, sf);
    let (out, os, _) = new_effect(a, c.site, 1);
    a.seed(os, fb);
    a.copy(out, c.ret);
  });
  a.rule("effect:Fiber.join", |a, c| {
    let fb = arg(a, c, 0);
    let v = load(a, fb, "~success");
    let (out, os, _) = new_effect(a, c.site, 1);
    a.copy(v, os);
    a.copy(out, c.ret);
  });
  a.rule("effect:Fiber.await", |a, c| {
    let fb = arg(a, c, 0);
    let v = load(a, fb, "~success");
    let w = wrap(a, c.site, v);
    let (out, os, _) = new_effect(a, c.site, 1);
    a.copy(w, os);
    a.copy(out, c.ret);
  });
  a.rule("effect:Fiber.getCurrent", |a, c| {
    let fb = fiber(a);
    a.copy(fb, c.ret);
  });
  a.rule("effect:Effect.runPromise", |a, c| {
    let e = self_effect(a, c);
    let (_, pv, value) = new_promise(a, c.site, 12);
    let s = success_of(a, e);
    a.copy(s, value);
    run_options(a, c, 1);
    a.copy(pv, c.ret);
  });
  a.rule("effect:Effect.runPromiseExit", |a, c| {
    let e = self_effect(a, c);
    let (_, pv, value) = new_promise(a, c.site, 12);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    a.copy(w, value);
    run_options(a, c, 1);
    a.copy(pv, c.ret);
  });
  a.rule("effect:Effect.runSync", |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    a.copy(s, c.ret);
    run_options(a, c, 1);
  });
  a.rule("effect:Effect.runSyncExit", |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    a.copy(w, c.ret);
    run_options(a, c, 1);
  });
  a.rule("effect:Effect.runFork", |a, c| {
    let e = self_effect(a, c);
    let fb = a.site_obj(c.site, 11, |_| ObjKind::Fiber);
    let s = success_of(a, e);
    let sf = a.field(fb, &ArcStr::from("~success"));
    a.copy(s, sf);
    run_options(a, c, 1);
    a.seed(c.ret, fb);
  });
  a.rule("effect:Effect.runCallback", |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    call_callbacks(a, c, 1, &[w]);
    run_options(a, c, 1);
    a.seed(c.ret, a.top_host);
  });
  dual!(a, "effect:Effect.bind", 3, |a, c| {
    let e = self_effect(a, c);
    let f = arg(a, c, 2);
    let s = success_of(a, e);
    let r = invoke(a, c.site, f, None, vec![s]);
    let (out, os, of) = new_effect(a, c.site, 1);
    let (obj, ov) = new_object(a, c.site, 13);
    a.constrain(s, Constraint::SpreadInto(obj));
    let rs = success_of(a, r);
    let elem = a.elem(obj);
    a.copy(rs, elem);
    a.copy(r, elem);
    a.copy(ov, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    let rf = failure_of(a, r);
    a.copy(rf, of);
    a.copy(out, c.ret);
  });
  dual!(a, "effect:Effect.bindTo", 2, |a, c| {
    let e = self_effect(a, c);
    let s = success_of(a, e);
    let (out, os, of) = new_effect(a, c.site, 1);
    let (obj, ov) = new_object(a, c.site, 13);
    let elem = a.elem(obj);
    a.copy(s, elem);
    a.copy(ov, os);
    let fl = failure_of(a, e);
    a.copy(fl, of);
    a.copy(out, c.ret);
  });
  a.rule("effect:Effect.log", |a, c| {
    for x in args_from(a, c, 0) {
      let (Arg::Var(v) | Arg::Spread(v)) = x;
      a.constrain(v, Constraint::Protocol(super::heap::Protocol::Inspect));
    }
    let (out, _, _) = new_effect(a, c.site, 1);
    a.copy(out, c.ret);
  });

  // == Layer

  a.rule("effect:Layer.succeed", |a, c| {
    if c.args.len() == 1 {
      partial(a, c, "effect:Layer.succeed.keyed");
      return;
    }
    let key = arg(a, c, 0);
    let value = arg(a, c, 1);
    let (layer, keys) = new_layer(a, c.site, 14);
    key_strings(a, key, keys);
    provide(a, key, value);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.succeed.keyed", |a, c| {
    // Through `partial`: args are [value, key].
    let value = arg(a, c, 0);
    let key = arg(a, c, 1);
    let (layer, keys) = new_layer(a, c.site, 14);
    key_strings(a, key, keys);
    provide(a, key, value);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.effect", |a, c| {
    if c.args.len() == 1 {
      partial(a, c, "effect:Layer.effect.keyed");
      return;
    }
    let key = arg(a, c, 0);
    let eff = arg(a, c, 1);
    let (layer, keys) = new_layer(a, c.site, 14);
    key_strings(a, key, keys);
    let value = success_of(a, eff);
    provide(a, key, value);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.effect.keyed", |a, c| {
    let eff = arg(a, c, 0);
    let key = arg(a, c, 1);
    let (layer, keys) = new_layer(a, c.site, 14);
    key_strings(a, key, keys);
    let value = success_of(a, eff);
    provide(a, key, value);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.sync", |a, c| {
    if c.args.len() == 1 {
      partial(a, c, "effect:Layer.sync.keyed");
      return;
    }
    let key = arg(a, c, 0);
    let f = arg(a, c, 1);
    let (layer, keys) = new_layer(a, c.site, 14);
    key_strings(a, key, keys);
    let value = invoke(a, c.site, f, None, vec![]);
    provide(a, key, value);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.sync.keyed", |a, c| {
    let f = arg(a, c, 0);
    let key = arg(a, c, 1);
    let (layer, keys) = new_layer(a, c.site, 14);
    key_strings(a, key, keys);
    let value = invoke(a, c.site, f, None, vec![]);
    provide(a, key, value);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.succeedContext", |a, c| {
    let ctx = arg(a, c, 0);
    let (layer, keys) = new_layer(a, c.site, 14);
    context_keys(a, ctx, keys);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.effectContext", |a, c| {
    let eff = arg(a, c, 0);
    let (layer, keys) = new_layer(a, c.site, 14);
    let ctx = success_of(a, eff);
    context_keys(a, ctx, keys);
    a.constrain(ctx, Constraint::Provision);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.syncContext", |a, c| {
    let f = arg(a, c, 0);
    let (layer, keys) = new_layer(a, c.site, 14);
    let ctx = invoke(a, c.site, f, None, vec![]);
    context_keys(a, ctx, keys);
    a.constrain(ctx, Constraint::Provision);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.effectDiscard", |a, c| {
    let (layer, _) = new_layer(a, c.site, 14);
    a.copy(layer, c.ret);
  });
  for name in ["effect:Layer.merge", "effect:Layer.mergeAll", "effect:Layer.provideMerge"] {
    a.rule(name, |a, c| {
      let (layer, keys) = new_layer(a, c.site, 14);
      for x in args_from(a, c, 0) {
        match x {
          Arg::Var(v) => {
            context_keys(a, v, keys);
            a.constrain(v, Constraint::Provision);
          }
          Arg::Spread(v) => {
            let e = elem_of(a, v);
            context_keys(a, e, keys);
            a.constrain(e, Constraint::Provision);
          }
        }
      }
      a.copy(layer, c.ret);
    });
  }
  dual!(a, "effect:Layer.provide", 2, |a, c| {
    let self_ = arg(a, c, 0);
    let that = arg(a, c, 1);
    let (layer, keys) = new_layer(a, c.site, 14);
    context_keys(a, self_, keys);
    a.constrain(that, Constraint::Provision);
    a.copy(layer, c.ret);
  });
  dual!(a, "effect:Layer.updateService", 3, |a, c| {
    let self_ = arg(a, c, 0);
    let key = arg(a, c, 1);
    let f = arg(a, c, 2);
    let current = lookup(a, key);
    let r = invoke(a, c.site, f, None, vec![current]);
    provide(a, key, r);
    let (layer, keys) = new_layer(a, c.site, 14);
    context_keys(a, self_, keys);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.passthrough", |a, c| {
    let self_ = arg(a, c, 0);
    let (layer, keys) = new_layer(a, c.site, 14);
    context_keys(a, self_, keys);
    let h = host_var(a);
    call_callbacks(a, c, 1, &[h, h]);
    a.copy(layer, c.ret);
  });
  for name in ["effect:Layer.catch", "effect:Layer.tap", "effect:Layer.flatMap"] {
    a.rule(name, |a, c| {
      let self_ = arg(a, c, 0);
      let (layer, keys) = new_layer(a, c.site, 14);
      context_keys(a, self_, keys);
      let (ctx, ckeys) = new_context(a, c.site, 15);
      context_keys(a, self_, ckeys);
      let h = host_var(a);
      let r = call_callbacks(a, c, 1, &[ctx, h]);
      context_keys(a, r, keys);
      a.constrain(r, Constraint::Provision);
      a.copy(layer, c.ret);
    });
  }
  a.rule("effect:Layer.unwrap", |a, c| {
    let eff = arg(a, c, 0);
    let (layer, keys) = new_layer(a, c.site, 14);
    let inner = success_of(a, eff);
    context_keys(a, inner, keys);
    a.constrain(inner, Constraint::Provision);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.suspend", |a, c| {
    let f = arg(a, c, 0);
    let r = invoke(a, c.site, f, None, vec![]);
    let (layer, keys) = new_layer(a, c.site, 14);
    context_keys(a, r, keys);
    a.constrain(r, Constraint::Provision);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.span", |a, c| {
    let (layer, keys) = new_layer(a, c.site, 14);
    let k = a.str(PARENT_SPAN_KEY);
    a.seed(keys, k);
    a.copy(layer, c.ret);
  });
  a.rule("effect:Layer.launch", |a, c| {
    let (out, _, _) = new_effect(a, c.site, 1);
    a.copy(out, c.ret);
  });
  a.rule("effect:Layer.build", |a, c| {
    let layer = arg(a, c, 0);
    let (ctx, keys) = new_context(a, c.site, 15);
    context_keys(a, layer, keys);
    let (out, os, _) = new_effect(a, c.site, 1);
    a.copy(ctx, os);
    a.copy(out, c.ret);
  });
  a.rule("effect:Layer.fromBuild", |a, c| {
    let f = arg(a, c, 0);
    let memo = memo_map(a);
    let scope = host_var(a);
    let r = invoke(a, c.site, f, None, vec![memo, scope]);
    let (layer, keys) = new_layer(a, c.site, 14);
    let ctx = success_of(a, r);
    context_keys(a, ctx, keys);
    a.constrain(ctx, Constraint::Provision);
    a.copy(layer, c.ret);
  });
  a.rule("effect:MemoMap.make", |a, c| {
    let m = memo_map(a);
    a.copy(m, c.ret);
  });
  a.rule("effect:ManagedRuntime.make", |a, c| {
    let layer = arg(a, c, 0);
    let rt = a.site_obj(c.site, 16, |a| {
      let keys = a.var();
      ObjKind::Runtime { keys }
    });
    let ObjKind::Runtime { keys } = a.objs[rt].kind else { unreachable!() };
    context_keys(a, layer, keys);
    a.constrain(layer, Constraint::Provision);
    a.seed(c.ret, rt);
  });
  a.rule("Runtime.runPromise", |a, c| {
    let e = arg(a, c, 0);
    let (_, pv, value) = new_promise(a, c.site, 12);
    let s = success_of(a, e);
    a.copy(s, value);
    run_options(a, c, 1);
    a.copy(pv, c.ret);
  });
  a.rule("Runtime.runPromiseExit", |a, c| {
    let e = arg(a, c, 0);
    let (_, pv, value) = new_promise(a, c.site, 12);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    a.copy(w, value);
    a.copy(pv, c.ret);
  });
  a.rule("Runtime.runSync", |a, c| {
    let e = arg(a, c, 0);
    let s = success_of(a, e);
    a.copy(s, c.ret);
  });
  a.rule("Runtime.runSyncExit", |a, c| {
    let e = arg(a, c, 0);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    a.copy(w, c.ret);
  });
  a.rule("Runtime.runFork", |a, c| {
    let e = arg(a, c, 0);
    let fb = a.site_obj(c.site, 11, |_| ObjKind::Fiber);
    let s = success_of(a, e);
    let sf = a.field(fb, &ArcStr::from("~success"));
    a.copy(s, sf);
    a.seed(c.ret, fb);
  });
  a.rule("Runtime.runCallback", |a, c| {
    let e = arg(a, c, 0);
    let s = success_of(a, e);
    let fl = failure_of(a, e);
    let both = a.var();
    a.copy(s, both);
    a.copy(fl, both);
    let w = wrap(a, c.site, both);
    call_callbacks(a, c, 1, &[w]);
    a.seed(c.ret, a.top_host);
  });
  a.rule("Runtime.context", |a, c| {
    let rt = this_of(a, c);
    let (ctx, keys) = new_context(a, c.site, 15);
    context_keys(a, rt, keys);
    let (_, pv, value) = new_promise(a, c.site, 12);
    a.copy(ctx, value);
    a.copy(pv, c.ret);
  });
  a.rule("Runtime.contextEffect:get", |a, c| {
    let rt = this_of(a, c);
    let (ctx, keys) = new_context(a, c.site, 15);
    context_keys(a, rt, keys);
    let (out, os, _) = new_effect(a, c.site, 1);
    a.copy(ctx, os);
    a.copy(out, c.ret);
  });
  a.rule("Runtime.memoMap:get", |a, c| {
    let m = memo_map(a);
    a.copy(m, c.ret);
  });

  // == Function and Pipeable

  a.rule("effect:Function.dual", |a, c| {
    let arity = arg(a, c, 0);
    let body = arg(a, c, 1);
    let (obj, data) = new_object(a, c.site, 17);
    let af = a.field(obj, &ArcStr::from("arity"));
    a.copy(arity, af);
    let bf = a.field(obj, &ArcStr::from("body"));
    a.copy(body, bf);
    let id = a.rule_id("effect:dual.fn").unwrap();
    let b = a.builtin(id, Some(data), None);
    a.seed(c.ret, b);
  });
  a.rule("effect:dual.fn", |a, c| {
    let data = this_of(a, c);
    let body = load(a, data, "body");
    let av = load(a, data, "arity");
    let arity = a.pts(av).iter().find_map(|o| match a.objs[*o].kind {
      ObjKind::Num(n) => Some(n as usize),
      _ => None,
    });
    let n = c.args.len();
    let full = match arity {
      Some(k) => n >= k || c.has_spread(),
      None => true,
    };
    let partial_ = match arity {
      Some(k) => n < k && !c.has_spread(),
      None => true,
    };
    if full {
      let full_args = args_from(a, c, 0);
      let r = invoke_args(a, c.site, body, None, full_args);
      a.copy(r, c.ret);
    }
    if partial_ {
      let (obj, pdata) = new_object(a, c.site, 18);
      let bf = a.field(obj, &ArcStr::from("body"));
      a.copy(body, bf);
      let (_, captured, elem) = new_array(a, c.site, 19);
      for x in args_from(a, c, 0) {
        match x {
          Arg::Var(v) => a.copy(v, elem),
          Arg::Spread(v) => a.constrain(v, Constraint::ElemOf(elem)),
        }
      }
      let cf = a.field(obj, &ArcStr::from("captured"));
      a.copy(captured, cf);
      // Keep the captured arguments positionally too, for the common small arities.
      for (i, x) in c.args.iter().enumerate() {
        if let Arg::Var(v) = x {
          let f = a.field(obj, &ArcStr::from(format!("a{i}")));
          a.copy(*v, f);
        }
      }
      let id = a.rule_id("effect:dual.partial").unwrap();
      let b = a.builtin_full(
        id,
        Some(pdata),
        (!c.has_spread()).then(|| u32::try_from(n).unwrap_or(0)),
        None,
      );
      a.seed(c.ret, b);
    }
  });
  a.rule("effect:dual.partial", |a, c| {
    let data = this_of(a, c);
    let body = load(a, data, "body");
    let n = a.builtin_arity(c);
    let self_ = arg(a, c, 0);
    let mut args = vec![Arg::Var(self_)];
    match n {
      Some(n) => {
        for i in 0..n {
          let f = load(a, data, &format!("a{i}"));
          args.push(Arg::Var(f));
        }
      }
      None => {
        let captured = load(a, data, "captured");
        args.push(Arg::Spread(captured));
      }
    }
    let r = invoke_args(a, c.site, body, None, args);
    a.copy(r, c.ret);
  });
  a.rule("effect:Function.pipe", |a, c| {
    let mut acc = arg(a, c, 0);
    for x in args_from(a, c, 1) {
      match x {
        Arg::Var(f) => acc = invoke(a, c.site, f, None, vec![acc]),
        Arg::Spread(fs) => {
          let e = elem_of(a, fs);
          let next = a.var();
          a.copy(acc, next);
          let r = invoke(a, c.site, e, None, vec![next]);
          a.copy(r, next);
          acc = next;
        }
      }
    }
    a.copy(acc, c.ret);
  });
  a.rule("effect:Function.flow", |a, c| {
    let (obj, data) = new_object(a, c.site, 20);
    for (i, x) in c.args.iter().enumerate() {
      if let Arg::Var(v) = x {
        let f = a.field(obj, &ArcStr::from(format!("f{i}")));
        a.copy(*v, f);
      }
    }
    let id = a.rule_id("effect:flow.fn").unwrap();
    let b = a.builtin_full(id, Some(data), Some(u32::try_from(c.args.len()).unwrap_or(0)), None);
    a.seed(c.ret, b);
  });
  a.rule("effect:flow.fn", |a, c| {
    let data = this_of(a, c);
    let n = a.builtin_arity(c).unwrap_or(0);
    let mut acc: Option<VarId> = None;
    for i in 0..n {
      let f = load(a, data, &format!("f{i}"));
      acc = Some(match acc {
        None => {
          let first_args = args_from(a, c, 0);
          invoke_args(a, c.site, f, c.this, first_args)
        }
        Some(prev) => invoke(a, c.site, f, None, vec![prev]),
      });
    }
    if let Some(r) = acc {
      a.copy(r, c.ret);
    }
  });
  a.rule("effect:Function.constant", |a, c| {
    let v = arg(a, c, 0);
    let id = a.rule_id("effect:constant.fn").unwrap();
    let b = a.builtin(id, Some(v), None);
    a.seed(c.ret, b);
  });
  a.rule("effect:constant.fn", |a, c| {
    let v = this_of(a, c);
    a.copy(v, c.ret);
  });
  a.rule("effect:Function.apply", |a, c| {
    let v = arg(a, c, 0);
    let id = a.rule_id("effect:apply.fn").unwrap();
    let b = a.builtin(id, Some(v), None);
    a.seed(c.ret, b);
  });
  a.rule("effect:apply.fn", |a, c| {
    let v = this_of(a, c);
    let f = arg(a, c, 0);
    let r = invoke(a, c.site, f, None, vec![v]);
    a.copy(r, c.ret);
  });
  a.rule("effect:Function.SK", |a, c| {
    let v = arg(a, c, 1);
    a.copy(v, c.ret);
  });
  a.rule("effect:Pipeable.pipe", |a, c| {
    // `self.pipe(f, g)` is `g(f(self))`: each step gets only the previous step's result, so
    // one step's input (say, an Effect's Context) does not reach the others. A spread runs
    // its functions an unknown number of times, so its step loops on its own result.
    let mut acc = this_of(a, c);
    for x in args_from(a, c, 0) {
      let next = a.var();
      match x {
        Arg::Var(f) => {
          let r = invoke(a, c.site, f, None, vec![acc]);
          a.copy(r, next);
        }
        Arg::Spread(v) => {
          let f = elem_of(a, v);
          a.copy(acc, next);
          let r = invoke(a, c.site, f, None, vec![next]);
          a.copy(r, next);
        }
      }
      acc = next;
    }
    a.copy(acc, c.ret);
  });
  a.rule("effect:Pipeable.pipeArguments", |a, c| {
    let self_ = arg(a, c, 0);
    let fns = arg(a, c, 1);
    let e = elem_of(a, fns);
    let acc = a.var();
    a.copy(self_, acc);
    let r = invoke(a, c.site, e, None, vec![acc]);
    a.copy(r, acc);
    a.copy(acc, c.ret);
  });

  // == default for other exports of the modelled modules

  a.rule("effect:default", |a, c| {
    // Sound by construction: the result yields every argument-derived value, and callbacks
    // receive those values plus the runtime objects Effect hands out. Calling the result falls
    // to the `Wrap` call path in the heap, so no per-result callable is minted here.
    let derived = a.var();
    // Callbacks get the derived values and the current Fiber (whose `context` is the ambient
    // Context when read); handing them a Context directly would turn every later call into
    // an API without a model into a bail.
    let fb = fiber(a);
    let w = wrap(a, c.site, derived);
    for x in args_from(a, c, 0) {
      let v = match x {
        Arg::Var(v) | Arg::Spread(v) => v,
      };
      a.copy(v, derived);
      let s = success_of(a, v);
      a.copy(s, derived);
      let f = failure_of(a, v);
      a.copy(f, derived);
      let aw = await_of(a, v);
      a.copy(aw, derived);
      // Everything inside argument objects (handlers, options, records of effects).
      let inner = a.var();
      a.constrain(v, Constraint::Contents(inner));
      a.copy(inner, derived);
      // Only functions run; a wrapped value or an effect among the arguments is not called.
      let callable = a.var();
      a.constrain(v, Constraint::Callables(callable));
      a.constrain(inner, Constraint::Callables(callable));
      let r = invoke(a, c.site, callable, None, vec![w, fb]);
      a.copy(r, derived);
      let rs = success_of(a, r);
      a.copy(rs, derived);
      let rf = failure_of(a, r);
      a.copy(rf, derived);
    }
    let (out, os, of) = new_effect(a, c.site, 21);
    a.copy(w, os);
    a.copy(w, of);
    a.copy(out, c.ret);
    a.copy(w, c.ret);
  });
}

fn memo_map(a: &mut Analysis) -> VarId {
  let m = a.site_obj(
    Site { module: rolldown_common::ModuleIdx::from_raw(0), start: 1, sub: 0 },
    251,
    |_| ObjKind::MemoMap,
  );
  let v = a.var();
  a.seed(v, m);
  v
}

/// `RunOptions.onFiberStart(fiber)` and friends.
fn run_options(a: &mut Analysis, c: &Call, i: usize) {
  if c.args.len() > i {
    let opts = arg(a, c, i);
    let fb = fiber(a);
    let on_start = load(a, opts, "onFiberStart");
    invoke(a, c.site, on_start, None, vec![fb]);
  }
}

/// Builds the object a `Spec` binds to.
pub fn spec_object(a: &mut Analysis, spec: Spec) -> ObjId {
  match spec {
    Spec::Rule(name) => {
      let id = a.rule_id(name).unwrap_or_else(|| panic!("unknown rule {name}"));
      a.builtin(id, None, None)
    }
    Spec::EffectValue => {
      let s = a.var();
      let f = a.var();
      a.alloc(ObjKind::Effect { success: s, failure: f }, None)
    }
    Spec::Key(name) => {
      let strings = a.var();
      let s = a.str(name);
      a.seed(strings, s);
      a.alloc(
        ObjKind::Key {
          strings,
          reference: name != SCOPE_KEY && name != PARENT_SPAN_KEY && name != MEMO_MAP_KEY,
        },
        None,
      )
    }
    Spec::FiberEffect => {
      let s = a.var();
      let f = a.var();
      let fb = fiber(a);
      a.copy(fb, s);
      a.alloc(ObjKind::Effect { success: s, failure: f }, None)
    }
    Spec::PipeablePrototype => pipeable_prototype(a),
    Spec::PipeableClass => {
      let proto = pipeable_prototype(a);
      let prototype = a.var();
      a.seed(prototype, proto);
      a.alloc(ObjKind::Class { ctor: None, heritage: None, prototype }, None)
    }
    Spec::EmptyLayer => {
      let keys = a.var();
      a.alloc(ObjKind::Layer { keys }, None)
    }
  }
}

pub fn default_object(a: &mut Analysis) -> ObjId {
  let id = a.rule_id("effect:default").unwrap();
  a.default_rule = Some(id);
  a.builtin(id, None, None)
}

/// A key declared by `Context.Service(...)`, `Context.Service<..>()(...)` or
/// `Context.Reference(...)`: its string, if literal, and whether it is a Reference.
pub fn key_declaration(init: &oxc::ast::ast::Expression<'_>) -> Option<(Option<String>, bool)> {
  use oxc::ast::ast::{Argument, Expression};
  let Expression::CallExpression(call) = init else { return None };
  let reference = match &call.callee {
    Expression::StaticMemberExpression(m) if m.property.name.as_str() == "Service" => false,
    Expression::StaticMemberExpression(m) if m.property.name.as_str() == "Reference" => true,
    Expression::CallExpression(inner) => match &inner.callee {
      Expression::StaticMemberExpression(m)
        if m.property.name.as_str() == "Service" && inner.arguments.is_empty() =>
      {
        false
      }
      _ => return None,
    },
    _ => return None,
  };
  let name = match call.arguments.first() {
    Some(Argument::StringLiteral(s)) => s.value.as_str().map(str::to_string),
    Some(Argument::TemplateLiteral(t)) if t.expressions.is_empty() => t
      .quasis
      .first()
      .and_then(|q| q.value.cooked.as_ref().and_then(|c| c.as_str().map(str::to_string))),
    _ => None,
  };
  Some((name, reference))
}

/// A key object for a declaration found in a modelled effect module.
pub fn declared_key(a: &mut Analysis, name: Option<&str>, reference: bool) -> ObjId {
  let strings = a.var();
  match name {
    Some(n) => {
      let s = a.str(n);
      a.seed(strings, s);
      a.effect_key(&ArcStr::from(n));
      if reference {
        a.never_prune.insert(ArcStr::from(n));
      }
    }
    None => a.seed(strings, a.top_host),
  }
  a.alloc(ObjKind::Key { strings, reference }, None)
}
/// The one `Pipeable.Prototype` object: `{ pipe() { return pipeArguments(this, arguments) } }`.
fn pipeable_prototype(a: &mut Analysis) -> ObjId {
  let id = a.rule_id("effect:Pipeable.pipe").expect("registered");
  let pipe = a.builtin(id, None, None);
  let site = Site { module: rolldown_common::ModuleIdx::from_raw(0), start: u32::MAX, sub: 0 };
  let obj = a.site_obj(site, 0, |_| ObjKind::Object);
  let field = a.field(obj, &ArcStr::from("pipe"));
  a.seed(field, pipe);
  obj
}
