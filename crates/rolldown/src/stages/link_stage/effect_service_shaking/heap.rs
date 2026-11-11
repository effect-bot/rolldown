//! Abstract heap and inclusion-based solver.
//!
//! Values are abstract objects (`ObjId`): allocation sites, constant strings,
//! model objects of the Effect runtime, opaque values. Variables (`VarId`) hold
//! sets of objects. Constraints attached to a variable fire for every object
//! that enters it, so the whole analysis is one monotone worklist over
//! inclusion edges. Nothing in this file knows what Effect is: model objects
//! are opaque kinds whose behaviour lives in the rule functions of
//! `effect.rs` and `host.rs`.

#![expect(clippy::print_stderr, reason = "stderr is the opt-in debug report")]

use arcstr::ArcStr;
use oxc_index::IndexVec;
use rolldown_common::ModuleIdx;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

oxc_index::define_index_type! {
  pub struct VarId = u32;
}
oxc_index::define_index_type! {
  pub struct ObjId = u32;
}
oxc_index::define_index_type! {
  pub struct FnId = u32;
}

/// A source position, used to cache the model objects a rule allocates per call site.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Site {
  pub module: ModuleIdx,
  pub start: u32,
  /// Distinguishes the calls a rule makes internally from the call it models.
  pub sub: u32,
}

/// The clone key of every wrap callback call (see `clone_for`).
const WRAP_CLONE_SITE: Site = Site { module: ModuleIdx::from_raw_unchecked(0), start: 0, sub: 0 };

/// Tag of the sub-sites rules make (bit 31 marks the end-tagged sites the generator makes).
pub(super) const RULE_SUB: u32 = 0x4000_0000;

/// The model slots of a Fiber: what its effect succeeds and fails with.
pub const FIBER_SUCCESS: &str = "~success";
pub const FIBER_FAILURE: &str = "~failure";
/// Tag of the call sites inside a function clone (both bits); the low bits index `clone_sites`.
pub(super) const CLONE_SUB: u32 = 0xC000_0000;

impl Site {
  /// A sub-site for a call a rule makes on `callee` on behalf of this call.
  ///
  /// Calls a rule makes from a sub-site stay on that sub-site: rules mint fresh variables each
  /// time they run, so a site per variable would let a rule that reaches itself (`pipe` handed
  /// `pipe`) mint memo entries without end. On one site such calls merge into the first
  /// invocation of each callee (`rule_memo`), which keeps the constraint set finite.
  pub fn sub(self, callee: VarId) -> Site {
    if self.sub & CLONE_SUB == RULE_SUB {
      return self;
    }
    Site {
      module: self.module,
      start: self.start,
      sub: RULE_SUB | (callee.raw().wrapping_add(1) & !CLONE_SUB),
    }
  }
}

pub type Rule = fn(&mut Analysis, &Call);

/// `Analysis::clone_origins`: clone to (template, call site), clone site index to (template
/// site, clone).
pub type CloneOrigins = (FxHashMap<FnId, (FnId, Site)>, FxHashMap<u32, (Site, FnId)>);

/// A row of the debug callback census: (module, site, mask) to (runs, largest slot).
type CallbackRow = ((u32, u32, u8), (usize, usize));

/// `ObjKind::Typed::export` of a value that is not an export.
pub const NO_EXPORT: u32 = u32::MAX;

/// An export of a type-modelled `effect` module: its `.d.ts` (if any), export name and module.
pub struct TypedExport {
  pub dts: std::path::PathBuf,
  pub name: ArcStr,
  pub module: ModuleIdx,
}

/// What a call to a type-modelled export with a given argument count reads from its `.d.ts`.
#[derive(Clone)]
struct TypedSig {
  params: Vec<Option<u32>>,
  rest: Option<u32>,
  ret: super::dts::RetShape,
  /// The call may store an argument in a value another argument holds, and may hand back what
  /// a container holds (`Dts::container_shape`).
  stores: bool,
  reads: bool,
  /// Per parameter, and for the rest parameter: whether its declared type mentions a type
  /// parameter (`Dts::generic_positions`). Only generic positions join the slot.
  generic: Vec<bool>,
  rest_generic: bool,
  /// Per parameter: which parameters of the callbacks it holds take values of a type parameter
  /// (`Dts::callback_masks`).
  masks: Vec<u8>,
  /// Per parameter: every callback it admits is declared to return a value that holds no
  /// service (`Dts::callback_free_returns`).
  free_returns: Vec<bool>,
  /// Per argument: a value of a type parameter whose constraint names no member, so effect's
  /// code reads none of its members by name (`Dts::opaque_positions`).
  opaque: Vec<bool>,
}

/// A stand-in for the service-free values a type-modelled call site is handed (direction 2's
/// admission filter, `Analysis::admit`): every access path on it yields itself, and anything that
/// could write into or hand over what it stands for (a store, an escape, a call with arguments)
/// taints it, after which those values flow as themselves.
struct StandIn {
  /// The values it stands for, and the variable each was held back from.
  held: Vec<(ObjId, VarId)>,
  /// The values it stands for, as a variable: uses that only read or hand them over (a walk,
  /// an escape, a conversion, a spread) apply to them through it.
  held_var: VarId,
  tainted: bool,
  /// Called (`stand_in_called`): a record it holds that reaches code taints it.
  called: bool,
  /// What calls through it return besides itself: the admitted values they were handed.
  returns: VarId,
}

/// Work the admission filter holds back until the object may hold a service
/// (`Analysis::may_hold`); re-checked whenever the solve settles.
enum Deferred {
  /// A walk of the object.
  Walk(ObjId, Walk, VarId, VarId, Site),
  /// Host adoption of the object (`adopt`).
  Adopt(ObjId, VarId),
  /// The object at a concrete position of a type-modelled call (`typed_concrete`).
  Concrete(ObjId, u32, Site),
}

/// The `.d.ts` bounds of a default-modelled export: which arguments are at `unknown`/`any`
/// positions, by argument count, and its callbacks' parameter types.
#[derive(Clone, Default)]
pub struct DefaultBounds {
  pub ignored: Vec<Vec<bool>>,
  pub callbacks: Option<Vec<Option<u32>>>,
}

/// The type bounds of a function: a declared type per parameter, for the return value and for
/// the elements of its rest parameter, as indices into `Analysis::tys` (`None` is open).
#[derive(Clone)]
struct FnBounds {
  params: Vec<Option<u32>>,
  ret: Option<u32>,
  rest: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Flavour {
  /// From an external or unresolvable module, or an entry's caller: may be anything, including
  /// Effects built by another copy of `effect`.
  Ext,
  /// From a builtin or host API: plain data as far as Effect is concerned.
  Host,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Protocol {
  /// `JSON.stringify`: reads `toJSON` on every reachable object.
  Json,
  /// String and number conversion: `toString`, `valueOf`, `Symbol.toPrimitive`.
  Primitive,
  /// `console.*` and `util.inspect`: `[util.inspect.custom]` on every reachable object.
  Inspect,
}

/// Effect's own formatting and structural equality (issue, "When a method is kept"): they walk
/// a value's keys, and the walk keeps nothing. What the walk reaches flows on to it, and the
/// protocol methods it calls on each object are read and run like any call (`Analysis::walk`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Walk {
  /// What reaches an `unknown`/`any` position of Effect's public API (`Analysis::ignored_pool`):
  /// the implicit protocol methods Effect and the host may call on such a value (issue,
  /// assumption 2's protocol exception), and a Date's `toISOString`.
  /// `[Redactable.symbolRedactable]` receives the current Context; every other argument is a
  /// primitive.
  Protocol,
  /// `Hash.hash`: `[Hash.symbol]`, and a Date's `getTime`/`toISOString` or a RegExp's
  /// `toString`.
  Hash,
  /// `Equal.equals`: `[Equal.symbol]`, which receives the walked values that have one
  /// (`compareStructure` calls it only when both sides do). Both sides are hashed too.
  Equal,
  /// `JSON.stringify` (`Protocol::Json`): `toJSON`, with the key.
  Json,
  /// `console.*` and `util.inspect` (`Protocol::Inspect`): `[util.inspect.custom]`, with the
  /// depth, the options and `util.inspect`.
  Inspect,
}

/// `Symbol.for("~effect/Redactable")`, `Equal.symbol` and `Hash.symbol` as model keys.
pub const REDACTABLE_KEY: &str = "@@for:~effect/Redactable";
pub const EQUAL_KEY: &str = "~effect/Equal";
pub const HASH_KEY: &str = "~effect/Hash";
/// `HttpServerRespondable.symbol`.
pub const RESPONDABLE_KEY: &str = "~effect/http/HttpServerRespondable";
/// Host methods a protocol walk need not call (direction 2). Each one calls no function and
/// coerces nothing, so it runs no program code, and what it returns is a primitive or an
/// iterator over the receiver's own elements, which the walk visits itself. Its call returns a
/// primitive. Anything else is called: other host methods dispatch to program code
/// (`Function.prototype.call` calls its receiver, `Array.prototype.map` its argument,
/// `Array.prototype.join` the elements' `toString`, `slice` the receiver's species).
/// `Array.prototype.*` names stand for the same methods of Maps and Sets too.
const INERT_PROTOCOL_METHODS: &[&str] = &[
  "String.prototype.toString",
  "Array.prototype.length",
  "Array.prototype.size",
  "Array.prototype.has",
  "Array.prototype.includes",
  "Array.prototype.indexOf",
  "Array.prototype.lastIndexOf",
  "Array.prototype.values",
  "Array.prototype.keys",
  "Array.prototype.entries",
  "Array.prototype.@@iterator",
];
/// The site of an escape into type-modelled `effect` code (admission, callbacks, summaries'
/// receivers): what that code writes back or passes on is a value effect's code made
/// (`Analysis::top_effect`), not an arbitrary unknown.
pub const EFFECT_ESCAPE: Site =
  Site { module: ModuleIdx::from_raw_unchecked(0), start: u32::MAX - 99, sub: 0 };
/// How a value crossed into class code (`Constraint::ClassEscape`), for the report.
pub const CLASS_HANDED: u8 = 0;
pub const CLASS_STORED: u8 = 1;
pub const CLASS_REFERENCED: u8 = 2;
pub const CLASS_TYPED_NEW: u8 = 3;
pub const CLASS_WIDENED: u8 = 4;
pub const CLASS_UNFOLLOWED: u8 = 5;
pub const CLASS_HOW: [&str; 6] = [
  "is handed to class code",
  "is stored in a class, its prototype or an instance",
  "is referenced from a class body",
  "is handed to the constructor of a class effect exports",
  "is in a set of values too large to track one by one",
  "is handed to program code the pass does not follow (a hand-written thenable)",
];
/// Effect's TypeId, the member its `Effect` type requires.
pub const EFFECT_TYPE_ID: &str = "~effect/Effect";

impl Walk {
  /// The methods the walk calls on each object, and how many arguments each receives (each
  /// one the walk's argument variable).
  fn protocols(self) -> &'static [(&'static str, usize)] {
    match self {
      Walk::Protocol => &[
        ("toJSON", 1),
        ("toString", 0),
        ("valueOf", 0),
        ("@@toPrimitive", 1),
        (super::host::INSPECT_CUSTOM, 3),
        (EQUAL_KEY, 1),
        (HASH_KEY, 0),
        (REDACTABLE_KEY, 1),
        (RESPONDABLE_KEY, 0),
        ("toISOString", 0),
      ],
      Walk::Hash => &[(HASH_KEY, 0), ("getTime", 0), ("toISOString", 0), ("toString", 0)],
      Walk::Equal => &[(EQUAL_KEY, 1)],
      Walk::Json => &[("toJSON", 1)],
      Walk::Inspect => &[(super::host::INSPECT_CUSTOM, 3)],
    }
  }
}

#[derive(Clone, Copy, Debug)]
pub enum FnKind {
  Normal,
  Arrow,
  Generator,
  Async,
  AsyncGenerator,
}

#[expect(clippy::struct_excessive_bools)]
pub struct FnInfo {
  pub params: Vec<VarId>,
  pub rest: Option<VarId>,
  pub this: VarId,
  pub ret: VarId,
  /// Failures of the effects a generator yields.
  pub yielded_fail: VarId,
  /// Values a generator yields (`yield v`, and the effects of `yield* e`), seen by `next()`.
  pub yielded: VarId,
  /// Elements of the `arguments` object.
  pub arguments_elem: VarId,
  pub kind: FnKind,
  /// The body only returns a function literal: calling it has no effect besides allocating.
  pub returns_closure_only: bool,
  pub closure: Option<ObjId>,
  pub activated: bool,
  /// A lazy top-level statement whose constraints are not generated yet (deferred generation,
  /// implementation.md). The solver asks its generator for them when it first installs the
  /// body, then clears this: a body is generated at most once per solve.
  pub deferred: bool,
  /// Constraints installed when the body becomes reachable.
  pub pending: Vec<Pending>,
  pub module: ModuleIdx,
  pub span_start: u32,
  /// Variables that belong to this function's body (temporaries, parameters, local
  /// bindings), renamed in a clone. Anything not listed is shared with the clone.
  pub locals: Vec<VarId>,
  /// No function or class is created in the body.
  pub leaf: bool,
  /// Calls get a copy of the body per call site (see `Analysis::clone_for`); `pending` is
  /// kept as the template.
  pub cloneable: bool,
  /// For arrows, the `this` of the enclosing function.
  pub enclosing_this: Option<VarId>,
  /// Number of clones made of this function.
  pub clones: u32,
}

impl FnInfo {
  pub fn new(
    module: ModuleIdx,
    span_start: u32,
    kind: FnKind,
    [this, ret, yielded_fail, yielded, arguments_elem]: [VarId; 5],
  ) -> Self {
    Self {
      params: vec![],
      rest: None,
      this,
      ret,
      yielded_fail,
      yielded,
      arguments_elem,
      kind,
      returns_closure_only: false,
      closure: None,
      activated: false,
      deferred: false,
      pending: vec![],
      module,
      span_start,
      locals: vec![],
      leaf: true,
      cloneable: false,
      enclosing_this: None,
      clones: 0,
    }
  }
}

#[derive(Clone)]
pub enum Pending {
  Copy(VarId, VarId),
  Constraint(VarId, Constraint),
  Seed(VarId, ObjId),
  Activate(FnId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
  Name(ArcStr),
  Var(VarId),
  Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyUse {
  /// Services of the key flow into the variable.
  Lookup(VarId),
  /// The variable's objects become services of the key.
  Provide(VarId),
  /// The key strings flow into the variable, as `Str` objects.
  Strings(VarId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Constraint {
  Load {
    key: Key,
    dst: VarId,
  },
  Store {
    key: Key,
    src: VarId,
  },
  /// On a key variable: load through each key object from the objects in `base`.
  LoadThrough {
    base: VarId,
    dst: VarId,
  },
  /// On a key variable: store through each key object into the objects in `base`.
  StoreThrough {
    base: VarId,
    src: VarId,
  },
  Call(Arc<Call>),
  SuccessOf(VarId),
  FailureOf(VarId),
  KeyUse(KeyUse),
  /// On a key's name: its `key` strings, or the `key` field of a key-shaped value
  /// (`Analysis::key_name_use`).
  KeyNameUse(KeyUse),
  /// On a container's keys, for an API without a model (`Analysis::contents_of`).
  ContentsLookup(VarId),
  /// On a Context: the service whose key *name* the variable holds (`getOrUndefinedUnsafe`).
  LookupNameIn {
    key: VarId,
    dst: VarId,
  },
  /// On a class's heritage variable: the class (`ObjId`) is a service key only when what it
  /// directly extends is a key object (`Analysis::key_heritage`).
  KeyHeritage(ObjId, KeyUse),
  /// On the heritage variable of a class another class extends: once it reaches a key, at any
  /// depth, the subclass is read as an unknown key (`Analysis::subclass_heritage`).
  SubclassHeritage(KeyUse),
  /// Keys held by Contexts, Layers and runtimes flow into the variable as `Str` objects.
  ContextKeys(VarId),
  /// Elements of arrays and other iterables.
  ElemOf(VarId),
  /// Everything an object holds (fields and elements), for values handed to an API without a
  /// model; plain objects are then demanded in full.
  Contents(VarId),
  /// Only the callable objects (closures, classes, builtins) of the variable.
  Callables(VarId),
  /// The name is read on the objects of the variable (no value flows): demand only.
  Demand(ArcStr),
  /// Some unknown name is read on the objects of the variable: demand everything.
  DemandAll,
  /// The settled value of promises, other objects unchanged.
  AwaitOf(VarId),
  Escape(Flavour, Option<Site>),
  Enumerate,
  Protocol(Protocol),
  /// Fields of objects are copied into the target object (`Object.assign`, spread).
  SpreadInto(ObjId),
  /// Objects in the variable are spread into every object that enters this (target) variable.
  SpreadTarget(VarId),
  /// The prototype of objects flows into the variable.
  ProtoOf(VarId),
  /// The variable's objects become the prototype of objects entering this variable.
  SetProto(VarId),
  /// Objects in a Layer/Context position: opaque ones may provide any key.
  Provision,
  /// On the Context argument of `Context.get(ctx, key)` and friends: the services of `key`
  /// flow into `dst` when `ctx` may be a Context (`Analysis::lookup_in`).
  LookupIn {
    key: VarId,
    dst: VarId,
  },
  /// On the receiver of `o.name(...)` whose callee is code the pass can't see: hand over the
  /// receivers that can hold that callee (`Analysis::escape_receiver`).
  EscapeReceiver(Flavour, Option<Site>, ArcStr),
  /// On a property slot of `receiver`: if code the pass can't see reaches the slot, the
  /// receiver is handed over with it.
  EscapeIfOpaque(ObjId, Flavour, Option<Site>),
  /// On a prototype variable: the object inherits from the variable's objects, directly or
  /// through their own prototypes (`Analysis::inherit`).
  Inherits(ObjId),
  /// On the variable holding a class accessor's declaration target: the accessor runs with
  /// every object that inherits from the target as `this`, and a setter's parameter takes what
  /// is stored under its name on those objects (`Analysis::accessor`).
  Accessor(Accessor),
  /// Entries of a string-keyed map (`Context.makeUnsafe`): `Str` objects are its keys and flow
  /// into the variable; opaque values may be any string, so they name every key; containers (the
  /// `[key, value]` pairs) and other values cannot match a string key and are skipped.
  MapKeys(VarId),
  /// A type-bounded edge (`Analysis::bounded_copy`): the objects the declared type `ty` admits
  /// flow into `dst`; the others wait until a change to them could make them admissible.
  FilterCopy {
    dst: VarId,
    ty: u32,
  },
  /// On a variable whose objects a walk reaches (`Analysis::walk`).
  Walk {
    walk: Walk,
    /// Everything the walk reaches.
    pool: VarId,
    /// What the protocol methods that take an argument receive.
    arg: VarId,
    site: Site,
  },
  /// `obj` flows into `dst` once anything reaches the variable (a walked object that has the
  /// protocol method loaded into the variable).
  Witness(ObjId, VarId),
  /// An argument of a call into type-modelled `effect` code (`Analysis::typed_arg`): what it
  /// contributes to the call's slot. The flag: a callback among it is declared to return a
  /// value that holds no service.
  TypedArg(VarId, VarId, Site, u8, bool, bool),
  /// The receivers of `o.name(...)` where the callee is the summary (`typed_call`): a summary
  /// is its own receiver; another object is one only if its `name` may be the summary
  /// (`TypedReceiverIf`). Slot, back variable and site as for `TypedArg`.
  TypedReceiver(ObjId, ArcStr, VarId, VarId, Site),
  /// What `TypedReceiver` reads off a receiver (the second object): if the callee summary is
  /// among it, the receiver goes to the call as `TypedArg` does.
  TypedReceiverIf(ObjId, ObjId, VarId, VarId, Site),
  /// Values entering a type-modelled value's slot (`Analysis::slot_in`); the object is the
  /// stand-in for what the admission filter holds back.
  SlotIn(VarId, ObjId),
  /// A call reaching only the functions (closures, classes, builtins) of the variable.
  CallFn(Box<Call>),
  /// A protocol method call (`protocol_call`) with the type model on: a summary found under a
  /// protocol name is effect code run with the receiver as `this`, so the receiver goes to
  /// unseen code instead of the summary being called; anything else is called.
  ProtocolCall(Arc<Call>),
  /// What a callback handed to type-modelled code returns (`typed_callback`): it goes to unseen
  /// code, and so does what an Effect or promise among it succeeds with.
  EscapeOut,
  /// A record or function handed to effect's code whose functions go without what they return
  /// (`escape_with` with `ret` false): their returns are tracked where they go
  /// (`typed_callback`).
  EscapeHeld,
  /// A member of an options object handed to type-modelled code: a function effect code may
  /// call with the slot (`typed_callback`); a type-modelled function among them is routed as
  /// `pool_route` does.
  TypedNested(VarId, VarId, Site, u8),
  /// Direction 2's admission filter at a type-modelled call (`Analysis::admit_typed`).
  AdmitTyped(VarId, bool),
  /// An argument at a concrete position of a call into type-modelled code (its declared type,
  /// interned, mentions no type parameter; `u32::MAX` if unknown): it joins no slot, but its
  /// callbacks run and its declared members are read (`Analysis::typed_concrete`).
  TypedConcrete(u32, Site),
  /// Every key named by these strings keeps all its methods (a key handed to type-modelled
  /// code, which may provide or read it).
  BailKeys,
  /// On a key's names: a key handed across a boundary (`Analysis::hand_name`).
  HandName,
  /// The value crosses into the body of a class (`ObjId`), which the pass does not analyse
  /// (`Analysis::class_escape`); the `u8` is how, for the report (`CLASS_HOW`).
  ClassEscape(ObjId, u8),
  /// A reference from the body of a class (`ObjId`) to the binding this variable holds, read
  /// through the member names (`ns.sub.x`): a namespace is followed member by member, anything
  /// else crosses into the class whole.
  ClassRef(ObjId, Vec<ArcStr>),
  /// On a `Reflect.get`/`Reflect.set` target: if it is class-related, the receiver variable
  /// crosses into the class, whose accessors run with it as `this`.
  ClassReceiver(VarId),
  /// On the right side of `x instanceof R`: `R[Symbol.hasInstance](x)` runs.
  HasInstance(VarId, Site),
  /// A callable reaching an adopted object's `then` (`Analysis::adopt`): the object bails.
  ThenableBail(ObjId),
  /// A callable reaching an adopted object's `then`: the await yields an unknown value.
  ThenableAwait(VarId),
}

impl Key {
  fn remap(&self, v: &impl Fn(VarId) -> VarId) -> Key {
    match self {
      Key::Var(k) => Key::Var(v(*k)),
      other => other.clone(),
    }
  }
}

impl KeyUse {
  fn remap(&self, v: &impl Fn(VarId) -> VarId) -> KeyUse {
    match self {
      KeyUse::Lookup(x) => KeyUse::Lookup(v(*x)),
      KeyUse::Provide(x) => KeyUse::Provide(v(*x)),
      KeyUse::Strings(x) => KeyUse::Strings(v(*x)),
    }
  }
}

impl Constraint {
  /// The same constraint with variables renamed by `v` and call sites by `s` (for clones).
  fn remap(&self, v: &impl Fn(VarId) -> VarId, s: &mut impl FnMut(Site) -> Site) -> Constraint {
    use Constraint as C;
    match self {
      C::Load { key, dst } => C::Load { key: key.remap(v), dst: v(*dst) },
      C::Store { key, src } => C::Store { key: key.remap(v), src: v(*src) },
      C::LoadThrough { base, dst } => C::LoadThrough { base: v(*base), dst: v(*dst) },
      C::StoreThrough { base, src } => C::StoreThrough { base: v(*base), src: v(*src) },
      C::Call(call) => C::Call(Arc::new(Call {
        site: s(call.site),
        this: call.this.map(v),
        args: call
          .args
          .iter()
          .map(|a| match a {
            Arg::Var(x) => Arg::Var(v(*x)),
            Arg::Spread(x) => Arg::Spread(v(*x)),
          })
          .collect(),
        ret: v(call.ret),
        is_new: call.is_new,
        builtin: call.builtin,
      })),
      C::SuccessOf(x) => C::SuccessOf(v(*x)),
      C::FailureOf(x) => C::FailureOf(v(*x)),
      C::KeyUse(u) => C::KeyUse(u.remap(v)),
      C::KeyNameUse(u) => C::KeyNameUse(u.remap(v)),
      C::ContentsLookup(d) => C::ContentsLookup(v(*d)),
      C::LookupNameIn { key, dst } => C::LookupNameIn { key: v(*key), dst: v(*dst) },
      C::KeyHeritage(class, u) => C::KeyHeritage(*class, u.remap(v)),
      C::SubclassHeritage(u) => C::SubclassHeritage(u.remap(v)),
      C::ContextKeys(x) => C::ContextKeys(v(*x)),
      C::ElemOf(x) => C::ElemOf(v(*x)),
      C::Contents(x) => C::Contents(v(*x)),
      C::Callables(x) => C::Callables(v(*x)),
      C::AwaitOf(x) => C::AwaitOf(v(*x)),
      C::SpreadTarget(x) => C::SpreadTarget(v(*x)),
      C::ProtoOf(x) => C::ProtoOf(v(*x)),
      C::SetProto(x) => C::SetProto(v(*x)),
      C::MapKeys(x) => C::MapKeys(v(*x)),
      C::FilterCopy { dst, ty } => C::FilterCopy { dst: v(*dst), ty: *ty },
      C::Walk { walk, pool, arg, site } => {
        C::Walk { walk: *walk, pool: v(*pool), arg: v(*arg), site: s(*site) }
      }
      C::Witness(obj, dst) => C::Witness(*obj, v(*dst)),
      C::ThenableBail(obj) => C::ThenableBail(*obj),
      C::ThenableAwait(dst) => C::ThenableAwait(v(*dst)),
      C::TypedArg(slot, back, site, mask, prim, opaque) => {
        C::TypedArg(v(*slot), v(*back), *site, *mask, *prim, *opaque)
      }
      C::TypedReceiver(callee, name, slot, back, site) => {
        C::TypedReceiver(*callee, name.clone(), v(*slot), v(*back), *site)
      }
      C::TypedReceiverIf(callee, recv, slot, back, site) => {
        C::TypedReceiverIf(*callee, *recv, v(*slot), v(*back), *site)
      }
      C::SlotIn(dst, stand_in) => C::SlotIn(v(*dst), *stand_in),
      C::TypedConcrete(ty, site) => C::TypedConcrete(*ty, *site),
      C::AdmitTyped(dst, prim) => C::AdmitTyped(v(*dst), *prim),
      C::TypedNested(slot, back, site, mask) => C::TypedNested(v(*slot), v(*back), *site, *mask),
      C::EscapeOut => C::EscapeOut,
      C::EscapeHeld => C::EscapeHeld,
      C::ProtocolCall(call) => match C::Call(Arc::clone(call)).remap(v, s) {
        C::Call(c) => C::ProtocolCall(c),
        _ => unreachable!(),
      },
      C::CallFn(call) => {
        let mut c = (**call).clone();
        c.this = c.this.map(v);
        c.ret = v(c.ret);
        c.args = c
          .args
          .iter()
          .map(|a| match a {
            Arg::Var(x) => Arg::Var(v(*x)),
            Arg::Spread(x) => Arg::Spread(v(*x)),
          })
          .collect();
        c.site = s(c.site);
        C::CallFn(Box::new(c))
      }
      C::BailKeys => C::BailKeys,
      C::HandName => C::HandName,
      C::ClassReceiver(r) => C::ClassReceiver(v(*r)),
      C::HasInstance(x, site) => C::HasInstance(v(*x), s(*site)),
      C::LookupIn { key, dst } => C::LookupIn { key: v(*key), dst: v(*dst) },
      C::Accessor(acc) => C::Accessor(Accessor {
        this: v(acc.this),
        setter: acc.setter.as_ref().map(|(name, param)| (name.clone(), v(*param))),
      }),
      C::Inherits(_)
      | C::ClassEscape(..)
      | C::ClassRef(..)
      | C::EscapeReceiver(..)
      | C::EscapeIfOpaque(..)
      | C::Demand(_)
      | C::DemandAll
      | C::Escape(..)
      | C::Enumerate
      | C::Protocol(_)
      | C::SpreadInto(_)
      | C::Provision => self.clone(),
    }
  }
}

/// A class getter or setter: its `this` variable, and for a setter its property name and
/// parameter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Accessor {
  pub this: VarId,
  pub setter: Option<(ArcStr, VarId)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Arg {
  Var(VarId),
  Spread(VarId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Call {
  pub site: Site,
  pub this: Option<VarId>,
  pub args: Vec<Arg>,
  pub ret: VarId,
  pub is_new: bool,
  /// The builtin object being called, for rules that carry static data on it.
  pub builtin: Option<ObjId>,
}

impl Call {
  pub fn has_spread(&self) -> bool {
    self.args.iter().any(|a| matches!(a, Arg::Spread(_)))
  }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Demand {
  #[default]
  None,
  Names(FxHashSet<ArcStr>),
  All,
}

impl Demand {
  pub fn add(&mut self, name: &ArcStr) {
    match self {
      Demand::None => *self = Demand::Names(FxHashSet::from_iter([name.clone()])),
      Demand::Names(names) => {
        names.insert(name.clone());
      }
      Demand::All => {}
    }
  }
  pub fn all(&mut self) {
    *self = Demand::All;
  }
  pub fn is_all(&self) -> bool {
    matches!(self, Demand::All)
  }
  pub fn has(&self, name: &str) -> bool {
    match self {
      Demand::None => false,
      Demand::Names(names) => names.contains(name),
      Demand::All => true,
    }
  }
  /// Merges `other` in; returns whether anything changed.
  pub fn merge(&mut self, other: &Demand) -> bool {
    match other {
      Demand::None => false,
      Demand::All => {
        let changed = !self.is_all();
        self.all();
        changed
      }
      Demand::Names(names) => {
        let before = match self {
          Demand::Names(mine) => mine.len(),
          Demand::None => 0,
          Demand::All => return false,
        };
        for n in names {
          self.add(n);
        }
        matches!(self, Demand::Names(mine) if mine.len() != before)
      }
    }
  }
}

#[derive(Clone, Debug)]
pub enum ObjKind {
  /// An object literal, class instance or `Object.create` result.
  Object,
  /// An array literal or array-like container (Map, Set, arguments).
  Array,
  Closure(FnId),
  Class {
    ctor: Option<FnId>,
    heritage: Option<VarId>,
    prototype: VarId,
  },
  /// A constant string or a well-known symbol used as a property key.
  Str(ArcStr),
  /// A service key; `strings` holds `Str` objects naming it (anything else: unknown).
  Key {
    strings: VarId,
    reference: bool,
  },
  /// Marker string object meaning "every key".
  AllKeys,
  /// A number, boolean, `null`, `undefined` or other value that holds no references.
  Prim,
  /// A small non-negative integer literal (arities, indices).
  Num(u32),
  Context {
    keys: VarId,
  },
  Layer {
    keys: VarId,
  },
  Effect {
    success: VarId,
    failure: VarId,
  },
  Fiber,
  MemoMap,
  Runtime {
    keys: VarId,
  },
  Promise {
    value: VarId,
  },
  /// A generator instance of the function.
  Iterator(FnId),
  /// Every access path yields the wrapped values.
  Wrap(VarId),
  /// A value made by `effect`'s type-modelled library code (`Analysis::typed_call`), or one of
  /// its exports. `slot` holds what the caller handed it at type-parameter positions; `export`
  /// indexes `Analysis::typed_exports` for an export (`NO_EXPORT` otherwise).
  Typed {
    slot: VarId,
    export: u32,
  },
  Top(Flavour),
  /// A callable with built-in behaviour; `data` is rule-specific.
  Builtin {
    rule: u16,
    data: Option<VarId>,
    arity: Option<u32>,
    this: Option<VarId>,
    target: Option<u16>,
  },
  /// The namespace object of a bundled module.
  Namespace,
  /// The namespace of an external module.
  External {
    flavour: Flavour,
    name: ArcStr,
  },
}

pub struct Obj {
  pub kind: ObjKind,
  pub fields: FxHashMap<ArcStr, VarId>,
  pub elem: Option<VarId>,
  pub proto: Option<VarId>,
  pub spread_from: Vec<VarId>,
  pub demand: Demand,
  pub escaped: Option<Flavour>,
  /// Variables that read every field, so fields created later flow to them too.
  pub unknown_loads: Vec<VarId>,
  /// Variables whose contents are copied into every field created later.
  pub unknown_stores: Vec<VarId>,
  /// Objects this one is spread into (`{ ...self }`), for demand propagation.
  pub spread_into: Vec<ObjId>,
  pub site: Option<Site>,
  /// Protocol methods read by formatters that reached this object.
  pub protocols: u8,
  /// The prototype object of a host builtin (`Map.prototype`): writing to it changes what the
  /// models assume.
  pub builtin_proto: bool,
  /// For an array: what is stored under a key that may not be an index (`named_elem`).
  pub named_elem: Option<VarId>,
  /// The named reads made on this object so far, each `(name, dst)` once, in order
  /// (`Analysis::load_name`). Only the kinds whose reads go on to the elements, spread sources
  /// and prototype record them: what the object gains later is replayed into these reads
  /// (`elem`, `proto_var`, `spread_into`).
  pub named_loads: Option<Box<NamedLoads>>,
}

/// An object's named reads (`Obj::named_loads`).
pub type NamedLoads = indexmap::IndexSet<(ArcStr, VarId), rustc_hash::FxBuildHasher>;

#[derive(Default)]
pub struct Var {
  pub pts: FxHashSet<ObjId>,
  pub copies: FxHashSet<VarId>,
  /// In the order they were registered, each once: a constraint registered twice would re-fire
  /// on every object and could never settle in a prototype or spread cycle.
  pub constraints: indexmap::IndexSet<Constraint, rustc_hash::FxBuildHasher>,
  delta: Vec<ObjId>,
  queued: bool,
  /// Widened (`Analysis::widen`): it holds an opaque value instead of what flows in.
  widened: bool,
}

/// The budget's overrides from the environment (`ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_*`), read
/// once per analysis. An entry override takes precedence over `max_entries`.
struct EnvLimits {
  objects: usize,
  vars: usize,
  entries: Option<u64>,
}

impl EnvLimits {
  fn read() -> Self {
    fn var<T: std::str::FromStr>(name: &str) -> Option<T> {
      std::env::var(name).ok().and_then(|s| s.parse().ok())
    }
    Self {
      objects: var("ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_OBJECTS").unwrap_or(Analysis::MAX_OBJECTS),
      vars: var("ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_VARS").unwrap_or(Analysis::MAX_VARS),
      entries: var("ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_ENTRIES"),
    }
  }
}

/// What the walks of `Analysis::reaches` have visited: a stamp per object and variable, so
/// starting a walk costs nothing and a visit is an array access rather than a hash.
///
/// A walk that answers false has visited every object it reaches, and found none that counts.
/// Until the graph changes (`Analysis::graph_changed`), a later walk that meets one of those
/// objects again can skip it, so the walks of one unchanged graph share their stamp. A walk
/// that answers true stops early and leaves objects it has not explored marked, so it starts a
/// new stamp.
#[derive(Default)]
struct Marks {
  walk: u32,
  objs: Vec<u32>,
  vars: Vec<u32>,
  /// The graph the current stamp's negative walks explored (`Analysis::graph_version`).
  graph: (u64, usize),
  /// How many objects the walks of the current stamp have visited.
  visited: usize,
}

impl Marks {
  /// Prepares a walk over `objs` objects and `vars` variables of the graph `graph`.
  fn begin(&mut self, objs: usize, vars: usize, graph: (u64, usize)) {
    if self.graph != graph {
      self.graph = graph;
      self.restart();
    }
    if self.objs.len() < objs {
      self.objs.resize(objs, 0);
    }
    if self.vars.len() < vars {
      self.vars.resize(vars, 0);
    }
  }

  /// Forgets what earlier walks visited.
  fn restart(&mut self) {
    if self.walk == u32::MAX {
      self.walk = 0;
      self.objs.fill(0);
      self.vars.fill(0);
    }
    self.walk += 1;
    self.visited = 0;
  }

  /// Whether `o` is visited for the first time under this stamp; marks it visited.
  fn visit_obj(&mut self, o: ObjId) -> bool {
    std::mem::replace(&mut self.objs[o.index()], self.walk) != self.walk
  }

  /// Whether `v` is visited for the first time under this stamp; marks it visited.
  fn visit_var(&mut self, v: VarId) -> bool {
    std::mem::replace(&mut self.vars[v.index()], self.walk) != self.walk
  }
}

/// Counts of what the analysis holds (`Analysis::work`): variables, objects and entries;
/// constraints installed, and variables, constraints and functions queued for the solve;
/// constraints pending in function bodies; functions and their locals; object slots (fields, elements,
/// prototypes); and records keyed by site or object (method names, builtins, inheritance,
/// class crossings, hole relations, prefixed strings); and the prefix text the prefix tables
/// keep.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Work {
  pub vars: usize,
  pub objs: usize,
  pub entries: u64,
  pub constraints: usize,
  pub queued: usize,
  pub pending: usize,
  pub fns: usize,
  pub locals: usize,
  pub slots: usize,
  pub records: usize,
  pub prefix_text: usize,
}

#[expect(clippy::struct_excessive_bools, reason = "independent switches of the analysis")]
pub struct Analysis {
  pub vars: IndexVec<VarId, Var>,
  pub objs: IndexVec<ObjId, Obj>,
  pub fns: IndexVec<FnId, FnInfo>,
  pub rules: Vec<(&'static str, Rule)>,
  rule_index: FxHashMap<&'static str, u16>,
  strs: FxHashMap<ArcStr, ObjId>,
  builtins: FxHashMap<(u16, Option<VarId>, Option<VarId>), ObjId>,
  site_cache: FxHashMap<(Site, u8), ObjId>,
  /// One `Wrap` object per inner variable, so wrapping stays finite.
  wraps: FxHashMap<VarId, ObjId>,
  /// Per `Wrap` object: the variable whose callables run with the wrapped values. Calling a
  /// wrap copies its arguments there, so one constraint per wrap serves every call site.
  wrap_callbacks: FxHashMap<ObjId, VarId>,
  /// The sites wraps call their callbacks from (see `wrap_of`).
  wrap_sites: FxHashSet<Site>,
  /// One variable holding exactly one object, reused by every constraint that needs it.
  holders: FxHashMap<ObjId, VarId>,
  /// Closures whose default `prototype` object exists (`function_prototype`).
  default_prototypes: FxHashSet<ObjId>,
  /// `(heir, ancestor)` pairs of the prototype relation, its transitive closure included.
  inherits: FxHashSet<(ObjId, ObjId)>,
  /// The objects inheriting from each object.
  heirs: FxHashMap<ObjId, Vec<ObjId>>,
  /// The class accessors declared on each object.
  accessors: FxHashMap<ObjId, Vec<Accessor>>,
  /// Declared types from `.d.ts` files, by index (`Constraint::FilterCopy`).
  tys: Vec<super::dts::Ty>,
  /// The `.d.ts` bounds of default-modelled exports (`effect:default`).
  pub default_bounds: Vec<DefaultBounds>,
  /// The type `Effect` (`effect_type`), once interned.
  effect_ty: Option<u32>,
  /// For each variable receiving what an object run as an effect yields, the variable whose
  /// objects are read wholesale into it (`success_of`).
  effect_gates: FxHashMap<VarId, VarId>,
  /// Type bounds of the exported functions of the modules analysed under the service-constructor
  /// exception, by (module, span start):
  /// a type index per parameter and for the return value (`None` is open).
  fn_bounds: FxHashMap<(ModuleIdx, u32), FnBounds>,
  /// Exported functions whose bounds are resolved when first called: their `.d.ts`, export name,
  /// parameter count, and whether the function is the one the export returns.
  pub lazy_bounds: FxHashMap<(ModuleIdx, u32), (std::path::PathBuf, ArcStr, usize, bool)>,
  /// The `.d.ts` files read so far.
  pub dts: super::dts::Dts,
  /// Objects a bounded edge rejected, with the edges that wait for them: a later field,
  /// prototype, unknown store, spread source, accessor or escape can make them admissible.
  rejected: FxHashMap<ObjId, Vec<(u32, VarId)>>,
  /// Set when the analysis exceeded its allocation budget: everything is kept.
  pub budget_exceeded: bool,
  /// Set when the budget ran out generating a deferred body (`solve_with`), for the report.
  pub budget_exceeded_generating: bool,
  /// Where generation found the budget run out and stopped (`Gen::checkpoint`), leaving the
  /// statement it was in partial. The solve then stops at once, keeping everything.
  pub generation_stopped: Option<Site>,
  /// Set while a generator (`Gen`) runs. Once the budget is over, every variable and object
  /// generation has the analysis allocate is one sink (`sink_var`, `sink_obj`), so what the
  /// nodes it is inside do after a child returns allocates nothing, however deep they nest
  /// (`generation_exhausted`).
  pub(super) generating: bool,
  sink_var: Option<VarId>,
  sink_obj: Option<ObjId>,
  /// Set the first time generation finds the budget over (`generation_exhausted`) and never
  /// cleared. From then on what generation made may hold the sink in place of its own
  /// variables and objects, so the solve stops at once (`solve_with`), whatever the totals
  /// become afterwards: widening takes entries off them.
  exhausted: bool,
  /// What the analysis held when generation first found the budget over, for tests counting
  /// what it does afterwards.
  #[cfg(test)]
  pub(super) at_exhaustion: Option<Work>,
  /// Objects each walk pool has walked.
  walked: FxHashSet<(VarId, ObjId)>,
  /// The program-wide pools of the protocol and hashing walks (`Walk`).
  ignored_pool: Option<VarId>,
  prim_arg: Option<VarId>,
  inspect_args: Option<VarId>,
  /// The exports of type-modelled `effect` modules (`ObjKind::Typed`).
  pub typed_exports: Vec<TypedExport>,
  /// Signatures read for a type-modelled export, by (export, argument count).
  typed_sigs: FxHashMap<(u32, usize), TypedSig>,
  /// Signatures of the functions export calls return, by (origin, argument count).
  returned_sigs: FxHashMap<((u32, usize), usize), Option<TypedSig>>,
  /// The value a call into type-modelled code returns, and the variable of what the call is
  /// handed, by (site, callee).
  typed_results: FxHashMap<(Site, Option<ObjId>), (ObjId, VarId)>,
  /// (function, slot) pairs `slot_in` has run.
  typed_slot_fns: FxHashSet<(ObjId, VarId)>,
  /// Per hole class (a class body the pass does not analyse): the one opaque value standing
  /// for everything its code makes, and back.
  pub opaque_of: FxHashMap<ObjId, ObjId>,
  pub opaque_class: FxHashMap<ObjId, ObjId>,
  /// The classes whose opaque value reached a key position: a key, a key's name or a class's
  /// heritage (`boundary_key`). A module hole among them is analysed in the next solve.
  pub keyed_boundaries: FxHashSet<ObjId>,
  /// Objects that crossed into a class body: the class and how (`CLASS_HOW`), first only.
  pub class_escaped: FxHashMap<ObjId, (ObjId, u8)>,
  /// `Reflect.get`/`Reflect.set` receivers waiting on a target that may become class-related.
  pending_receivers: FxHashMap<ObjId, Vec<VarId>>,
  /// Class-related objects of each hole class: the class, its prototype, and their heirs.
  pub hole_related: FxHashMap<ObjId, ObjId>,
  /// Objects escaped into effect's code, and into code the pass can't see (`escape`).
  effect_escaped: FxHashSet<ObjId>,
  /// Functions handed to effect's code so far only where their return is declared primitive
  /// (`escape_with`): their return goes over at the first escape that hands it.
  ret_withheld: FxHashSet<ObjId>,
  genuine_escaped: FxHashSet<ObjId>,
  /// Keys handed across a boundary (`hand_key`): the objects handed, the key names they carry,
  /// whether one names every key, and the lookups through keys from a boundary.
  handed: FxHashSet<ObjId>,
  handed_names: FxHashSet<ArcStr>,
  handed_all: bool,
  handed_lookups: FxHashSet<VarId>,
  /// Strings known to start with a constant (`prefixed_host`): each is a host value of its own,
  /// so every rule reads it as host data, except a key's name, which names only the keys with
  /// that prefix. `prefix_lookups` are the lookups through such names, seen by keys named later.
  pub(super) prefixes: FxHashMap<ObjId, ArcStr>,
  prefix_hosts: FxHashMap<ArcStr, ObjId>,
  prefix_lookups: Vec<(ArcStr, VarId)>,
  /// The variables `admitted_values` filters into: they hold `top_effect` once something
  /// is held back from them, and primitives as themselves.
  filter_vars: FxHashSet<VarId>,
  /// The filter variables of `admitted_callables`.
  callable_filter_vars: FxHashSet<VarId>,
  callable_filters: FxHashMap<VarId, VarId>,
  /// Widening (`widen`): the set size past which a variable is widened, the boundary, the
  /// number of variables widened, and how many variables the analysis starts with.
  pub widen_limit: usize,
  /// The entries budget (`MAX_ENTRIES` unless the build sets one).
  pub max_entries: u64,
  env_limits: EnvLimits,
  widened_class: Option<ObjId>,
  /// The boundary of program code the pass does not follow (`unfollowed`).
  unfollowed_class: Option<ObjId>,
  /// Objects that crossed the widening boundary. `class_escaped` records only the first
  /// boundary (for the report); an object that crossed another one first crosses the widening
  /// boundary as well, since widening reads names more strongly (`hand_key`).
  widen_escaped: FxHashSet<ObjId>,
  pub widened_vars: usize,
  init_vars: usize,
  /// The member `TypedReceiver` reads off a wrapped value or a string, by receiver and name,
  /// shared by the call sites that read it (`receiver_member`).
  receiver_members: FxHashMap<(ObjId, ArcStr), VarId>,
  /// Test-only: a fresh member at every call site, the construction before `receiver_members`,
  /// for comparing the two.
  #[cfg(test)]
  fresh_receiver_members: bool,
  /// Diagnostic: what created each variable (`var_tag` at the time).
  pub var_origin: Vec<&'static str>,
  pub var_tag: &'static str,
  tag_names: FxHashMap<std::mem::Discriminant<Constraint>, &'static str>,
  clone_tags: FxHashMap<(ModuleIdx, u32), &'static str>,
  /// (result, receiver) pairs of method calls already joined (`typed_call`).
  typed_receivers: FxHashSet<(ObjId, ObjId)>,
  /// Each call's handed-values variable, translated for slots (`slot_in`).
  typed_backs: FxHashMap<VarId, VarId>,
  /// (object, slot) pairs `typed_arg` has handled.
  typed_args_seen: FxHashSet<(ObjId, VarId, bool, bool)>,
  /// The method summary read off a type-modelled value.
  typed_methods: FxHashMap<ObjId, ObjId>,
  /// The module each type-modelled call result comes from, for its container pool.
  typed_result_module: FxHashMap<ObjId, ModuleIdx>,
  /// The result of a call whose declared return is concrete (`RetShape::concrete`), by the
  /// same key as `typed_results`: a summary that holds nothing.
  typed_plain_results: FxHashMap<(Site, Option<ObjId>), ObjId>,
  /// What concrete-position callbacks receive: an unknown, service-free host value.
  concrete_args: Option<VarId>,
  /// (object, position type, site) triples `typed_concrete` has handled.
  concrete_seen: FxHashSet<(ObjId, u32)>,
  /// The export call (export, argument count) each type-modelled call result comes from: a
  /// call of the result is a call of the function that export returns.
  typed_result_origin: FxHashMap<ObjId, (u32, usize)>,
  /// One program-wide container pool per type-modelled module (direction 2): what calls that may
  /// store an argument hand it, read by every call into the module.
  typed_pools: FxHashMap<ModuleIdx, VarId>,
  /// The member names each type-modelled module declares only as methods
  /// (`Dts::method_only_names`).
  method_only: FxHashMap<ModuleIdx, std::sync::Arc<FxHashSet<String>>>,
  /// Whether method calls on each type-modelled module's values may store into them
  /// (`Dts::methods_store`).
  methods_store: FxHashMap<ModuleIdx, bool>,
  /// Debug: callbacks run (site, mask, slot, function).
  callback_log: Vec<(u32, u32, u8, VarId, ObjId)>,
  /// Debug: type-modelled call applications by source site.
  typed_call_counts: FxHashMap<(u32, u32), (usize, FxHashSet<u32>)>,
  /// Debug: why stand-ins were tainted.
  pub taint_log: FxHashMap<String, usize>,
  /// Debug: which calls write each pool.
  pool_writers: FxHashMap<ModuleIdx, FxHashSet<String>>,
  /// Argument variables of type-modelled calls: spread elements by source, and filtered copies
  /// by (variable, declared type).
  typed_spreads: FxHashMap<VarId, VarId>,
  typed_filtered: FxHashMap<(VarId, u32), VarId>,
  /// `record_fields` by object, and the program-wide sink of unread callback results.
  record_fields: FxHashMap<ObjId, VarId>,
  discarded: Option<VarId>,
  /// The type-modelled modules whose code the program calls: an export, a method read off one,
  /// or a value one of its calls returned.
  pub typed_called_modules: FxHashSet<ModuleIdx>,
  /// Stand-ins (`StandIn`), and the one of each type-modelled call site.
  stand_ins: FxHashMap<ObjId, StandIn>,
  site_stand_ins: FxHashMap<Site, ObjId>,
  /// The admission-filtered copy of each variable stored under an unknown key, with its stand-in.
  unknown_store_filters: FxHashMap<VarId, VarId>,
  /// Work held back by the admission filter (`Deferred`).
  deferred: Vec<Deferred>,
  /// Effects and promises handed to type-modelled code before they may hold a service
  /// (`admit_typed`), with their destinations; decided when the solve settles
  /// (`settle_admissions`).
  held_admissions: Vec<(ObjId, VarId)>,
  held_admissions_seen: FxHashSet<(ObjId, VarId)>,
  /// Objects known to hold something that may be a service (`may_hold` only grows).
  live: FxHashSet<ObjId>,
  /// The size of `never_prune` when `live` was last valid.
  live_never_prune: usize,
  /// Objects known to reach code (`reaches_code` only grows).
  live_code: FxHashSet<ObjId>,
  /// What the walks of `reaches` have visited, for `may_hold` and `reaches_code`.
  marks: [Marks; 2],
  /// Counts the changes to what `reaches` reads: points-to sets, the variables an object
  /// reaches through, accessors and keys (`graph_changed`).
  graph_version: u64,
  /// Admission counters for the report: admitted, held back, tainted stand-ins.
  pub admission_stats: (u64, u64, u64),
  /// What awaiting each summary gives (`AwaitOf` on a `Wrap`).
  awaited_wraps: FxHashMap<ObjId, VarId>,
  /// Each adopted object's `then` (`adopt`).
  thenable_then: FxHashMap<ObjId, VarId>,
  /// Hand-written thenables that have bailed (`adopt`).
  thenables_bailed: FxHashSet<ObjId>,

  pub hash_pool: Option<VarId>,
  /// The program-wide pools of host serialization and inspection (`host_protocol`).
  json_pool: Option<VarId>,
  inspect_pool: Option<VarId>,
  /// Objects whose primitive conversion methods have been called.
  primitive_converted: FxHashSet<ObjId>,
  /// The options the installed loggers are called with, once `Effect.log` is reached
  /// (`effect::run_loggers`).
  pub log_messages: Option<VarId>,
  /// The failure slot every model Effect shares. Services never enter the error channel
  /// (issue, "Assumptions" 3), so nothing reads it: `failure_of` yields nothing.
  failure_sink: VarId,
  /// Rule invocations, for diagnostics.
  pub rule_calls: FxHashMap<u16, usize>,
  /// One "any argument" variable per spread call, shared by every `arg` read of that call.
  pub spread_any: FxHashMap<(Site, VarId), VarId>,
  /// Function clones by (function, call site); see `clone_for`.
  clone_memo: FxHashMap<(FnId, Site), FnId>,
  /// Interned clone-tagged call sites: (site in the template, clone) -> index.
  clone_site_ids: FxHashMap<(Site, FnId), u32>,
  /// Per index: the template site and the clone depth of the function holding it.
  clone_sites: Vec<(Site, u8)>,
  rest_arrays: FxHashMap<FnId, ObjId>,
  /// Named loads by property name: (base variable, destination). A load wires only objects that
  /// have the field; when a store creates the field later, the registry wires the load then.
  loads_by_name: FxHashMap<ArcStr, FxHashSet<(VarId, VarId)>>,
  /// One rule invocation per (call site, builtin): later calls at the same site merge their
  /// receiver, arguments and result into the first. Keeps the constraint set finite, since a
  /// rule invocation creates variables and constraints.
  rule_memo: FxHashMap<(Site, ObjId), Call>,
  pub debug: bool,
  pub top_ext: ObjId,
  /// A value type-modelled `effect` code made (direction 2's slot filler). It is `top_ext` in
  /// every respect but one: in a key position it names only keys `effect` declares, since
  /// effect's code never names a user key (`effect_keys`).
  pub top_effect: ObjId,
  pub top_host: ObjId,
  pub prim: ObjId,
  pub all_keys: ObjId,
  /// A key of unknown name that code the pass can't see made: lookups and provides through it
  /// are as conservative as through `all_keys`, but handing it to type-modelled code bails
  /// nothing, since unseen code does not reach services through the runtime (issue, "Code the
  /// pass can't see").
  pub unseen_keys: ObjId,
  pub ambient_context: ObjId,
  pub key_values: FxHashMap<ArcStr, VarId>,
  pub unknown_key_values: VarId,
  pub all_values: VarId,
  pub bail_all: bool,
  pub bail_reasons: Vec<String>,
  /// Keys never pruned: References and the keys the fiber runtime reads itself.
  pub never_prune: FxHashSet<ArcStr>,
  /// Keys declared inside the `effect` package. A default-modelled effect value may provide or
  /// yield services of these keys only: effect's own code never names a user key.
  pub effect_keys: Vec<ArcStr>,
  /// Union of the values of `effect_keys`.
  pub effect_key_values: VarId,
  /// `Str` objects of `effect_keys`.
  pub effect_key_strings: VarId,
  /// Rule id of `effect:default`, once registered.
  pub default_rule: Option<u16>,
  /// Model Effects whose public type is `Exit`: only these have the Exit members.
  exits: FxHashSet<ObjId>,
  /// The property name of each `o.name(...)` call site, as generated (clones' sites are not
  /// listed and keep the general handling).
  pub method_names: FxHashMap<Site, ArcStr>,
  /// The lazy top-level statement behind each named export of a namespace object, and every
  /// lazy statement behind a namespace (activated by wholesale use).
  pub lazy_exports: FxHashMap<(ObjId, ArcStr), FnId>,
  pub lazy_namespaces: FxHashMap<ObjId, Vec<FnId>>,
  worklist: Vec<VarId>,
  /// Constraints registered since the last solver step, to be applied to the objects their
  /// variable already holds. Kept out of the call stack so constraint cycles cannot recurse.
  new_constraints: Vec<(VarId, usize)>,
  /// Functions whose pending constraints wait to be installed.
  activation_queue: Vec<FnId>,
  pub steps: u64,
  /// Running totals of points-to entries and copy edges, for the allocation budget.
  pub total_pts: u64,
  pub total_copies: u64,
  /// Rows of the objects' named-load indexes (`Obj::named_loads`), counted as entries: each is
  /// a read edge from an object to a variable, as a copy edge is.
  pub total_named_loads: u64,
  /// The loads replay queues on a spread source's holder (`spread_into`), counted as entries
  /// as each is queued: the solve applies one spread to every object its variable holds before
  /// it checks the budget again, so a row counted only once its load runs would come too late.
  pub total_replay_loads: u64,
  /// Replay or the named-load index found the analysis over budget (`replay_stopped`). Latched:
  /// the solve stops even if widening later takes the totals back under the cap.
  replay_over: bool,
}

pub const PROTO_JSON: u8 = 1;
pub const PROTO_PRIMITIVE: u8 = 2;
pub const PROTO_INSPECT: u8 = 4;

impl Analysis {
  pub fn new() -> Self {
    let mut vars: IndexVec<VarId, Var> = IndexVec::new();
    let mut objs: IndexVec<ObjId, Obj> = IndexVec::new();
    let mut alloc = |kind: ObjKind| objs.push(Obj::new(kind, None));
    let top_ext = alloc(ObjKind::Top(Flavour::Ext));
    let top_effect = alloc(ObjKind::Top(Flavour::Ext));
    let top_host = alloc(ObjKind::Top(Flavour::Host));
    let prim = alloc(ObjKind::Prim);
    let all_keys = alloc(ObjKind::AllKeys);
    let unseen_keys = alloc(ObjKind::AllKeys);
    let ambient_keys = vars.push(Var::default());
    let ambient_context = alloc(ObjKind::Context { keys: ambient_keys });
    let unknown_key_values = vars.push(Var::default());
    let all_values = vars.push(Var::default());
    let failure_sink = vars.push(Var::default());
    let effect_key_values = vars.push(Var::default());
    let effect_key_strings = vars.push(Var::default());
    let init_vars = vars.len();
    let mut this = Self {
      vars,
      objs,
      fns: IndexVec::new(),
      rules: vec![],
      rule_index: FxHashMap::default(),
      strs: FxHashMap::default(),
      builtins: FxHashMap::default(),
      site_cache: FxHashMap::default(),
      wraps: FxHashMap::default(),
      wrap_callbacks: FxHashMap::default(),
      wrap_sites: FxHashSet::default(),
      holders: FxHashMap::default(),
      default_prototypes: FxHashSet::default(),
      inherits: FxHashSet::default(),
      heirs: FxHashMap::default(),
      accessors: FxHashMap::default(),
      tys: vec![],
      effect_ty: None,
      default_bounds: vec![],
      effect_gates: FxHashMap::default(),
      fn_bounds: FxHashMap::default(),
      lazy_bounds: FxHashMap::default(),
      dts: super::dts::Dts::default(),
      rejected: FxHashMap::default(),
      budget_exceeded: false,
      budget_exceeded_generating: false,
      generation_stopped: None,
      generating: false,
      sink_var: None,
      sink_obj: None,
      exhausted: false,
      #[cfg(test)]
      at_exhaustion: None,
      walked: FxHashSet::default(),
      ignored_pool: None,
      prim_arg: None,
      inspect_args: None,
      awaited_wraps: FxHashMap::default(),
      thenable_then: FxHashMap::default(),
      thenables_bailed: FxHashSet::default(),
      typed_exports: vec![],
      typed_sigs: FxHashMap::default(),
      returned_sigs: FxHashMap::default(),
      typed_results: FxHashMap::default(),
      typed_methods: FxHashMap::default(),
      typed_result_module: FxHashMap::default(),
      typed_result_origin: FxHashMap::default(),
      typed_plain_results: FxHashMap::default(),
      concrete_args: None,
      concrete_seen: FxHashSet::default(),
      typed_pools: FxHashMap::default(),
      pool_writers: FxHashMap::default(),
      taint_log: FxHashMap::default(),
      typed_call_counts: FxHashMap::default(),
      callback_log: vec![],
      methods_store: FxHashMap::default(),
      method_only: FxHashMap::default(),
      typed_called_modules: FxHashSet::default(),
      typed_spreads: FxHashMap::default(),
      typed_filtered: FxHashMap::default(),
      record_fields: FxHashMap::default(),
      discarded: None,
      stand_ins: FxHashMap::default(),
      site_stand_ins: FxHashMap::default(),
      unknown_store_filters: FxHashMap::default(),
      deferred: vec![],
      held_admissions: vec![],
      held_admissions_seen: FxHashSet::default(),
      live: FxHashSet::default(),
      live_code: FxHashSet::default(),
      marks: Default::default(),
      graph_version: 0,
      live_never_prune: 0,
      admission_stats: (0, 0, 0),
      typed_args_seen: FxHashSet::default(),
      typed_backs: FxHashMap::default(),
      typed_receivers: FxHashSet::default(),
      typed_slot_fns: FxHashSet::default(),
      opaque_of: FxHashMap::default(),
      opaque_class: FxHashMap::default(),
      keyed_boundaries: FxHashSet::default(),
      class_escaped: FxHashMap::default(),
      pending_receivers: FxHashMap::default(),
      hole_related: FxHashMap::default(),
      effect_escaped: FxHashSet::default(),
      ret_withheld: FxHashSet::default(),
      genuine_escaped: FxHashSet::default(),
      handed: FxHashSet::default(),
      handed_names: FxHashSet::default(),
      handed_all: false,
      handed_lookups: FxHashSet::default(),
      prefixes: FxHashMap::default(),
      prefix_hosts: FxHashMap::default(),
      prefix_lookups: Vec::new(),
      filter_vars: FxHashSet::default(),
      callable_filter_vars: FxHashSet::default(),
      callable_filters: FxHashMap::default(),
      max_entries: Self::MAX_ENTRIES,
      env_limits: EnvLimits::read(),
      widen_limit: std::env::var("ROLLDOWN_EFFECT_SERVICE_SHAKING_WIDEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400),
      widened_class: None,
      unfollowed_class: None,
      widen_escaped: FxHashSet::default(),
      widened_vars: 0,
      receiver_members: FxHashMap::default(),
      #[cfg(test)]
      fresh_receiver_members: false,
      init_vars,
      var_origin: vec!["init"; init_vars],
      var_tag: "gen",
      tag_names: FxHashMap::default(),
      clone_tags: FxHashMap::default(),
      hash_pool: None,
      json_pool: None,
      inspect_pool: None,
      primitive_converted: FxHashSet::default(),
      log_messages: None,
      failure_sink,
      rule_calls: FxHashMap::default(),
      rule_memo: FxHashMap::default(),
      spread_any: FxHashMap::default(),
      clone_memo: FxHashMap::default(),

      clone_site_ids: FxHashMap::default(),
      clone_sites: vec![],
      rest_arrays: FxHashMap::default(),
      loads_by_name: FxHashMap::default(),
      debug: std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_DEBUG").is_some(),
      top_ext,
      top_effect,
      top_host,
      prim,
      all_keys,
      unseen_keys,
      ambient_context,
      key_values: FxHashMap::default(),
      unknown_key_values,
      all_values,
      bail_all: false,
      bail_reasons: vec![],
      never_prune: FxHashSet::default(),
      effect_keys: vec![],
      effect_key_values,
      effect_key_strings,
      default_rule: None,
      exits: FxHashSet::default(),
      method_names: FxHashMap::default(),
      lazy_exports: FxHashMap::default(),
      lazy_namespaces: FxHashMap::default(),
      worklist: vec![],
      new_constraints: vec![],
      activation_queue: vec![],
      steps: 0,
      total_pts: 0,
      total_copies: 0,
      total_named_loads: 0,
      total_replay_loads: 0,
      replay_over: false,
    };
    this.seed(ambient_keys, all_keys);
    // Unknown keys alias every key: what they provide is visible through every key.
    this.copy(unknown_key_values, all_values);
    this
  }

  // == construction

  pub fn var(&mut self) -> VarId {
    if self.generation_exhausted() {
      if let Some(v) = self.sink_var {
        return v;
      }
      let v = self.new_var();
      self.sink_var = Some(v);
      return v;
    }
    self.new_var()
  }

  fn new_var(&mut self) -> VarId {
    self.var_origin.push(self.var_tag);
    self.vars.push(Var::default())
  }

  pub fn alloc(&mut self, kind: ObjKind, site: Option<Site>) -> ObjId {
    if self.generation_exhausted() {
      if let Some(o) = self.sink_obj {
        return o;
      }
      let o = self.objs.push(Obj::new(ObjKind::Object, None));
      self.sink_obj = Some(o);
      return o;
    }
    self.objs.push(Obj::new(kind, site))
  }

  /// Generation ran past the budget: the solve will stop before installing anything more
  /// (`solve_with`), keeping everything, so what generation still does is only unwinding. From
  /// here every variable and object it allocates is the sink (`var`, `alloc`); no prototype
  /// slot is made for an object made before (`proto_var`), the one slot a node completing
  /// after its child makes on its own object; and the generator drops what it would add to the
  /// analysis or record about what it generated (`Gen::exhausted` lists where). Past the cap,
  /// a generator allocates at most the sink variable and object, wherever it is, and nothing in
  /// proportion to how deep it is when the cap is crossed or after a child stopped. The first
  /// time it is found over latches `exhausted`.
  #[inline]
  pub(super) fn generation_exhausted(&mut self) -> bool {
    if !self.generating {
      return false;
    }
    if !self.exhausted && self.over_budget() {
      self.exhausted = true;
      #[cfg(test)]
      {
        self.at_exhaustion = Some(self.work());
      }
    }
    self.exhausted
  }

  /// Generation ran past the budget (`generation_exhausted`): the analysis is terminal.
  pub(super) fn exhausted(&self) -> bool {
    self.exhausted
  }

  /// The solve will stop at once, keeping everything (`solve_with`): generation ran past the
  /// budget, or the analysis is over it. Nothing more needs setting up or generating.
  pub(super) fn terminal(&self) -> bool {
    self.exhausted || self.replay_over || self.over_budget()
  }

  /// Replay into earlier named loads and the named-load index stop here once the analysis is
  /// over budget (`Obj::named_loads`): the first time it is found over latches `replay_over`,
  /// so the solve stops keeping everything and a row or replay skipped here is never missed by
  /// a solve that finishes.
  #[inline]
  fn replay_stopped(&mut self) -> bool {
    if !self.replay_over && self.over_budget() {
      self.replay_over = true;
    }
    self.exhausted || self.replay_over
  }

  pub fn rule(&mut self, name: &'static str, rule: Rule) -> u16 {
    if let Some(id) = self.rule_index.get(name) {
      return *id;
    }
    self.rules.push((name, rule));
    let id = u16::try_from(self.rules.len() - 1).expect("too many rules");
    self.rule_index.insert(name, id);
    id
  }

  pub fn rule_id(&self, name: &str) -> Option<u16> {
    self.rule_index.get(name).copied()
  }

  pub fn builtin(&mut self, rule: u16, data: Option<VarId>, this: Option<VarId>) -> ObjId {
    if let Some(id) = self.builtins.get(&(rule, data, this)) {
      return *id;
    }
    let id = self.alloc(ObjKind::Builtin { rule, data, arity: None, this, target: None }, None);
    self.builtins.insert((rule, data, this), id);
    id
  }

  /// A builtin carrying static data: an argument count and a target rule.
  pub fn builtin_full(
    &mut self,
    rule: u16,
    data: Option<VarId>,
    arity: Option<u32>,
    target: Option<u16>,
  ) -> ObjId {
    self.alloc(ObjKind::Builtin { rule, data, arity, this: None, target }, None)
  }

  /// The data variable of the builtin being called. A memoised rule call sees a fresh copy of
  /// it as `this`, so a rule that writes into its data uses this instead.
  pub fn builtin_data(&self, call: &Call) -> Option<VarId> {
    match call.builtin.map(|b| &self.objs[b].kind) {
      Some(ObjKind::Builtin { data, .. }) => *data,
      _ => None,
    }
  }

  pub fn builtin_arity(&self, call: &Call) -> Option<u32> {
    match call.builtin.map(|b| &self.objs[b].kind) {
      Some(ObjKind::Builtin { arity, .. }) => *arity,
      _ => None,
    }
  }

  pub fn builtin_target(&self, call: &Call) -> Option<u16> {
    match call.builtin.map(|b| &self.objs[b].kind) {
      Some(ObjKind::Builtin { target, .. }) => *target,
      _ => None,
    }
  }

  pub fn num(&mut self, n: u32) -> ObjId {
    let key = ArcStr::from(format!("\u{0}num:{n}"));
    if let Some(id) = self.strs.get(&key) {
      return *id;
    }
    let id = self.alloc(ObjKind::Num(n), None);
    self.strs.insert(key, id);
    id
  }

  /// A variable holding exactly `obj`.
  pub fn holder(&mut self, obj: ObjId) -> VarId {
    if let Some(v) = self.holders.get(&obj) {
      return *v;
    }
    let v = self.var();
    self.holders.insert(obj, v);
    self.seed(v, obj);
    v
  }

  /// The builtin for a method of a model or host object. Shared by every receiver: the call
  /// supplies the receiver variable, so one rule invocation per call site covers every object
  /// the receiver may be, instead of one per object.
  pub fn method(&mut self, rule: u16, obj: ObjId) -> ObjId {
    let _ = obj;
    self.builtin(rule, None, None)
  }

  pub fn str(&mut self, s: &str) -> ObjId {
    if let Some(id) = self.strs.get(s) {
      return *id;
    }
    let atom = ArcStr::from(s);
    let id = self.alloc(ObjKind::Str(atom.clone()), None);
    self.strs.insert(atom, id);
    id
  }

  /// A model object allocated once per call site and tag.
  /// The objects `site_obj` made at a site whose offset is another object's id (instances
  /// and default prototypes, tags 200, 201 and 204), with that object. For the decision record.
  pub fn id_sited(&self) -> FxHashMap<ObjId, ObjId> {
    self
      .site_cache
      .iter()
      .filter(|((_, tag), _)| matches!(tag, 200 | 201 | 204))
      .map(|((site, _), o)| (*o, ObjId::from_usize(site.start as usize)))
      .collect()
  }

  /// For the decision record: each clone with its template and the call site it was made for
  /// (`clone_for`'s key), and each clone-tagged site's index with the template's site and the
  /// clone it belongs to.
  pub fn clone_origins(&self) -> CloneOrigins {
    let clones = self.clone_memo.iter().map(|((f, site), c)| (*c, (*f, *site))).collect();
    let sites = self.clone_site_ids.iter().map(|((site, f), i)| (*i, (*site, *f))).collect();
    (clones, sites)
  }

  /// The clone key shared by the calls of wrap callbacks (`clone_for`).
  pub fn is_wrap_clone_site(site: Site) -> bool {
    site == WRAP_CLONE_SITE
  }

  pub fn site_obj(
    &mut self,
    site: Site,
    tag: u8,
    make: impl FnOnce(&mut Self) -> ObjKind,
  ) -> ObjId {
    if let Some(id) = self.site_cache.get(&(site, tag)) {
      return *id;
    }
    let kind = make(self);
    let id = self.alloc(kind, Some(site));
    self.site_cache.insert((site, tag), id);
    id
  }

  pub fn new_effect(&mut self, site: Site, tag: u8) -> (ObjId, VarId, VarId) {
    let id = self.site_obj(site, tag, |a| {
      let success = a.var();
      let failure = a.failure_sink;
      ObjKind::Effect { success, failure }
    });
    let ObjKind::Effect { success, failure } = self.objs[id].kind else { unreachable!() };
    (id, success, failure)
  }

  pub fn key_value_var(&mut self, key: &ArcStr) -> VarId {
    if let Some(v) = self.key_values.get(key) {
      return *v;
    }
    let v = self.var();
    self.key_values.insert(key.clone(), v);
    self.graph_changed();
    // Lookups through unknown keys see every key, and every key sees what unknown keys provide.
    self.copy(v, self.all_values);
    self.copy(self.unknown_key_values, v);
    if self.effect_keys.contains(key) {
      self.copy(v, self.effect_key_values);
    }
    if self.handed_names.contains(key) {
      for dst in self.handed_lookups.clone() {
        self.copy(v, dst);
      }
    }
    let prefixed: Vec<VarId> = self
      .prefix_lookups
      .iter()
      .filter(|(prefix, _)| key.starts_with(prefix.as_str()))
      .map(|(_, dst)| *dst)
      .collect();
    for dst in prefixed {
      self.copy(v, dst);
    }
    v
  }

  /// A string the program builds from a constant prefix and run-time parts (`\`a/${x}\``,
  /// `'a/' + x`). It is host data like any run-time string: a host value (`Top(Host)`) of its
  /// own, one per prefix, so every rule that reads host values reads it the same way. As a key's
  /// name it names only the keys whose names start with the prefix (`prefix_key_use`).
  pub fn prefixed_host(&mut self, prefix: &str) -> ObjId {
    if let Some(obj) = self.prefix_hosts.get(prefix) {
      return *obj;
    }
    let prefix = ArcStr::from(prefix);
    let obj = self.alloc(ObjKind::Top(Flavour::Host), None);
    self.prefix_hosts.insert(prefix.clone(), obj);
    self.prefixes.insert(obj, prefix);
    obj
  }

  /// A key named by a string with a known prefix (`prefixed_host`). A lookup sees what is
  /// provided under every key with that prefix, now or named later, under names the pass does
  /// not know, and under effect's keys when one of them has the prefix. Providing through it,
  /// or reading its names, is read as for a key of any name.
  fn prefix_key_use(&mut self, prefix: &ArcStr, use_: &KeyUse) {
    let KeyUse::Lookup(dst) = use_ else {
      let all = self.all_keys;
      self.key_use(all, use_);
      return;
    };
    let dst = *dst;
    if self.prefix_lookups.iter().any(|(p, d)| *d == dst && p == prefix) {
      return;
    }
    self.prefix_lookups.push((prefix.clone(), dst));
    let named: Vec<VarId> = self
      .key_values
      .iter()
      .filter(|(name, _)| name.starts_with(prefix.as_str()))
      .map(|(_, v)| *v)
      .collect();
    for v in named {
      self.copy(v, dst);
    }
    self.copy(self.unknown_key_values, dst);
    if self.effect_keys.iter().any(|k| k.starts_with(prefix.as_str())) {
      self.copy(self.effect_key_values, dst);
    }
  }

  /// A value crossed a boundary (unseen code, effect's code, a host builtin, class code or a
  /// widened set): if it is a key or a key's name, a key from that boundary may be it, since
  /// code there may hand back what it was given. Values inside it cross on their own (fields,
  /// prototype, heritage), so only keys and names are read here; each is recorded once, whatever
  /// the flavour or the order of escapes.
  ///
  /// A host value may be a string only known at run time (`process.env.X`), which may name any
  /// key. `any_name` is set for the widening boundary: the widened set holds that value, and a
  /// lookup by its opaque element must still see every key; an object that crossed another
  /// class boundary before it reached the widened set crosses again (`class_escape`).
  /// A host value handed to code the pass can't see or doesn't follow is not read as a name:
  /// that code may make any string itself, and a runtime name string it hands back is treated
  /// the same (Tim's decision, 2026-11-09; fixture `external_returned_runtime_name`).
  /// Canonical keys and literal names it is handed are recorded above and do come back. A
  /// structural key (a `{ key }` record) is read only through the values its fields hold, so a
  /// runtime name in its `key` field is not tracked either, though the object handed back is the
  /// program's own (Tim's decision, 2026-11-09; fixture `external_returned_structural_key`).
  /// That exclusion rests on rarity alone: reading the `key` field of every record handed over
  /// makes Stripe 0% smaller. A literal-named structural key is still covered (its literal is
  /// handed as a `Str`). A host value that is a number or another primitive never equals a
  /// key's name (effect's maps are keyed by the name).
  fn hand_key(&mut self, obj: ObjId, any_name: bool) {
    match self.objs[obj].kind.clone() {
      ObjKind::Top(Flavour::Host) if any_name => self.hand_all(),
      ObjKind::Str(_) if self.handed.insert(obj) => self.hand_name(obj),
      ObjKind::Key { strings, .. } if self.handed.insert(obj) => {
        self.constrain(strings, Constraint::HandName);
      }
      _ => {}
    }
  }

  /// A handed key's name (`hand_key`): lookups through a key from a boundary see what is
  /// provided under it. A name the pass can't read (a primitive or host value) may be any key.
  fn hand_name(&mut self, obj: ObjId) {
    match self.objs[obj].kind.clone() {
      ObjKind::Str(name) => {
        if !self.handed_names.insert(name.clone()) {
          return;
        }
        if let Some(v) = self.key_values.get(&name).copied() {
          for dst in self.handed_lookups.clone() {
            self.copy(v, dst);
          }
        }
      }
      ObjKind::AllKeys if obj == self.all_keys => self.hand_all(),
      ObjKind::Num(_) | ObjKind::Prim | ObjKind::Top(Flavour::Host) => self.hand_all(),
      _ => {}
    }
  }

  fn hand_all(&mut self) {
    if std::mem::replace(&mut self.handed_all, true) {
      return;
    }
    for dst in self.handed_lookups.clone() {
      self.copy(self.all_values, dst);
    }
  }

  /// A lookup through a key from a boundary (`unseen_keys`) sees every key handed across one.
  fn handed_lookup(&mut self, dst: VarId) {
    if !self.handed_lookups.insert(dst) {
      return;
    }
    if self.handed_all {
      self.copy(self.all_values, dst);
    }
    let vars: Vec<VarId> =
      self.handed_names.iter().filter_map(|n| self.key_values.get(n).copied()).collect();
    for v in vars {
      self.copy(v, dst);
    }
  }

  /// Registers a key declared inside the `effect` package.
  pub fn effect_key(&mut self, key: &ArcStr) {
    if self.effect_keys.contains(key) {
      return;
    }
    self.effect_keys.push(key.clone());
    let s = self.str(key);
    self.seed(self.effect_key_strings, s);
    let v = self.key_value_var(key);
    self.copy(v, self.effect_key_values);
  }

  fn is_default(&self, obj: ObjId) -> bool {
    matches!(self.objs[obj].kind, ObjKind::Builtin { rule, .. } if Some(rule) == self.default_rule)
  }

  pub fn field(&mut self, obj: ObjId, name: &ArcStr) -> VarId {
    if let Some(v) = self.objs[obj].fields.get(name) {
      return *v;
    }
    let v = self.var();
    self.objs[obj].fields.insert(name.clone(), v);
    self.recheck_tree(obj);
    let loads = self.objs[obj].unknown_loads.clone();
    for dst in loads {
      self.copy(v, dst);
    }
    // A store under an unknown key never writes one of effect's own members (`~effect/...`):
    // only code naming that member does (`is_effect_member`).
    if !is_effect_member(name) {
      let stores = self.objs[obj].unknown_stores.clone();
      for src in stores {
        self.copy(src, v);
      }
    }
    // Named loads that saw this object before the field existed.
    if let Some(waiting) = self.loads_by_name.get(name) {
      let hits: Vec<VarId> = waiting
        .iter()
        .filter(|(base, _)| self.vars[*base].pts.contains(&obj))
        .map(|(_, dst)| *dst)
        .collect();
      for dst in hits {
        self.copy(v, dst);
      }
    }
    v
  }

  /// Reads a field without creating it: objects that lack the field contribute nothing until a
  /// store creates it, at which point `field` wires the registered loads.
  fn read_field(&mut self, obj: ObjId, name: &ArcStr, dst: VarId) {
    if let Some(f) = self.objs[obj].fields.get(name).copied() {
      self.copy(f, dst);
    }
  }

  /// An array's slot for stores under a key that may not be an index: a named read sees these,
  /// not the elements (`arr.then` can only be set by a store that may name `then`).
  pub fn named_elem(&mut self, obj: ObjId) -> VarId {
    if let Some(v) = self.objs[obj].named_elem {
      return v;
    }
    // Not replayed: a named read on an array creates this slot (`load_name`), so none ran
    // before it existed.
    let v = self.var();
    self.objs[obj].named_elem = Some(v);
    v
  }

  pub fn elem(&mut self, obj: ObjId) -> VarId {
    if let Some(v) = self.objs[obj].elem {
      return v;
    }
    let v = self.var();
    self.objs[obj].elem = Some(v);
    // Named reads that ran before the object had elements.
    for (name, dst) in self.named_loads(obj) {
      if self.replay_stopped() {
        break;
      }
      if reads_elem(&self.objs[obj].kind, &name) {
        self.copy(v, dst);
      }
    }
    v
  }

  /// The named reads made on `obj` so far (`Obj::named_loads`).
  fn named_loads(&self, obj: ObjId) -> Vec<(ArcStr, VarId)> {
    self.objs[obj].named_loads.as_deref().map_or_else(Vec::new, |l| l.iter().cloned().collect())
  }

  pub fn proto_var(&mut self, obj: ObjId) -> VarId {
    if let Some(v) = self.objs[obj].proto {
      return v;
    }
    if self.generation_exhausted() {
      return self.var();
    }
    let v = self.var();
    self.objs[obj].proto = Some(v);
    // Whatever becomes this object's prototype is inherited from by it, and by every object
    // that already inherits from it.
    self.constrain(v, Constraint::Inherits(obj));
    if let Some(heirs) = self.heirs.get(&obj).cloned() {
      for heir in heirs {
        self.constrain(v, Constraint::Inherits(heir));
      }
    }
    // Reads that ran before the object had a prototype read through it now.
    for (name, dst) in self.named_loads(obj) {
      if self.replay_stopped() {
        return v;
      }
      self.constrain(v, Constraint::Load { key: Key::Name(name), dst });
    }
    if records_named_loads(&self.objs[obj].kind) {
      for dst in self.objs[obj].unknown_loads.clone() {
        if self.replay_stopped() {
          return v;
        }
        self.constrain(v, Constraint::Load { key: Key::Unknown, dst });
      }
    }
    v
  }

  /// `heir` inherits from `ancestor`: the accessors declared on `ancestor` run on `heir`, and
  /// `heir` also inherits from `ancestor`'s own prototypes.
  fn inherit(&mut self, heir: ObjId, ancestor: ObjId) {
    if heir == ancestor || !self.inherits.insert((heir, ancestor)) {
      return;
    }
    self.heirs.entry(ancestor).or_default().push(heir);
    self.recheck(heir);
    // An object inheriting from a hole class or its prototype is that class's instance.
    if let Some(&class) = self.hole_related.get(&ancestor) {
      self.hole_related.insert(heir, class);
      self.class_escape(heir, class, CLASS_STORED);
    }
    if let Some(accessors) = self.accessors.get(&ancestor).cloned() {
      for acc in &accessors {
        self.run_accessor_on(heir, acc);
      }
    }
    if let Some(pv) = self.objs[ancestor].proto {
      self.constrain(pv, Constraint::Inherits(heir));
    }
  }

  /// A class accessor declared on `target`: it runs on every object inheriting from it.
  fn accessor(&mut self, target: ObjId, acc: &Accessor) {
    let list = self.accessors.entry(target).or_default();
    if list.contains(acc) {
      return;
    }
    list.push(acc.clone());
    self.graph_changed();
    self.recheck_tree(target);
    if let Some(heirs) = self.heirs.get(&target).cloned() {
      for heir in heirs {
        self.run_accessor_on(heir, acc);
      }
    }
  }

  fn run_accessor_on(&mut self, obj: ObjId, acc: &Accessor) {
    self.seed(acc.this, obj);
    if let Some((name, param)) = &acc.setter {
      // `obj.name = v` writes `obj`'s own slot in the model: the setter receives it.
      let field = self.field(obj, name);
      self.copy(field, *param);
    }
  }

  // == edges and seeds

  /// Something `reaches` reads has changed, so what earlier walks found holds no longer.
  pub fn graph_changed(&mut self) {
    self.graph_version += 1;
  }

  pub fn seed(&mut self, var: VarId, obj: ObjId) {
    if self.vars[var].widened && !self.opaque_class.contains_key(&obj) && !self.keeps_identity(obj)
    {
      let class = self.widen_class();
      self.class_escape(obj, class, CLASS_WIDENED);
      return;
    }
    if self.vars[var].pts.insert(obj) {
      self.graph_changed();
      self.total_pts += 1;
      self.vars[var].delta.push(obj);
      self.enqueue(var);
      if self.vars[var].pts.len() > self.widen_limit {
        self.widen(var);
      }
    }
  }

  /// Values a widened variable still holds as themselves: keys, classes, effect's key
  /// constructors and names that may be any key's (a host value, `process.env.X`, or every key
  /// name). There are few, and each may be or make or name a key: a lookup through one, a class
  /// extending one, or a call of a key constructor must see what it is, so the opaque value
  /// never stands for a key or an unknown name.
  fn keeps_identity(&self, obj: ObjId) -> bool {
    match self.objs[obj].kind {
      ObjKind::Key { .. }
      | ObjKind::Class { .. }
      | ObjKind::Top(Flavour::Host)
      | ObjKind::AllKeys => true,
      ObjKind::Builtin { rule, .. } => self.key_constructor(rule),
      _ => false,
    }
  }

  fn key_constructor(&self, rule: u16) -> bool {
    ["effect:Context.Service", "effect:Context.Reference"]
      .iter()
      .any(|r| self.rule_id(r) == Some(rule))
  }

  /// A value made by code the pass does not analyse but that runs in this runtime: a class
  /// hole's or a widened set's opaque value. In a key position it is a key made there or one
  /// handed to it (`hand_key`); a key constructor class code holds bails every key
  /// (`class_escape`).
  fn is_opaque(&self, obj: ObjId) -> bool {
    self.opaque_class.contains_key(&obj)
  }

  /// The value of program code the pass does not follow (a hand-written thenable's settlement,
  /// a property setter): an opaque value, as for a class hole.
  pub fn unfollowed(&mut self) -> ObjId {
    let class = self.unfollowed_class();
    self.opaque(class)
  }

  /// The boundary of program code the pass does not follow. It is apart from the widening
  /// boundary, which alone reads a host value handed to it as a name that may be any key's
  /// (`hand_key`).
  fn unfollowed_class(&mut self) -> ObjId {
    if let Some(c) = self.unfollowed_class {
      return c;
    }
    let c = self.alloc(ObjKind::Object, None);
    self.unfollowed_class = Some(c);
    self.opaque(c);
    c
  }

  /// The boundary widened values cross into (`widen`): an opaque value, as for a class hole.
  fn widen_class(&mut self) -> ObjId {
    if let Some(c) = self.widened_class {
      return c;
    }
    let c = self.alloc(ObjKind::Object, None);
    self.widened_class = Some(c);
    self.opaque(c);
    c
  }

  /// A variable whose set has grown past `widen_limit` stops tracking its values one by one.
  /// They cross into an opaque boundary (`class_escape`, as values handed to code that runs in
  /// this runtime but is not analysed): a service among them keeps every method, a key bails,
  /// an Effect's success, a function's returns and an object's fields cross too, and functions
  /// run with opaque arguments. The variable holds the boundary's opaque value instead, and
  /// what arrives later crosses as it arrives. Reads, calls and stores through the opaque value
  /// hand over what they are given and yield it again, so whatever consumes the variable stays
  /// sound. The sets pruning reads after the solve (each key's providers, the analysis's own
  /// roots) are never widened.
  fn widen(&mut self, var: VarId) {
    if var.index() < self.init_vars || self.key_values.values().any(|v| *v == var) {
      return;
    }
    self.graph_changed();
    self.vars[var].widened = true;
    self.widened_vars += 1;
    let class = self.widen_class();
    let (kept, objs): (FxHashSet<ObjId>, FxHashSet<ObjId>) =
      std::mem::take(&mut self.vars[var].pts).into_iter().partition(|o| self.keeps_identity(*o));
    self.total_pts -= objs.len() as u64;
    self.vars[var].pts = kept;
    let op = self.opaque(class);
    for o in objs {
      if o != op {
        self.class_escape(o, class, CLASS_WIDENED);
      }
    }
    self.vars[var].pts.insert(op);
    self.total_pts += 1;
    self.vars[var].delta.push(op);
    self.enqueue(var);
  }

  pub fn copy(&mut self, src: VarId, dst: VarId) {
    if src == dst || !self.vars[src].copies.insert(dst) {
      return;
    }
    self.total_copies += 1;
    let objs: Vec<ObjId> = self.vars[src].pts.iter().copied().collect();
    for obj in objs {
      self.seed(dst, obj);
    }
  }

  pub fn constrain(&mut self, var: VarId, constraint: Constraint) {
    let (index, new) = self.vars[var].constraints.insert_full(constraint);
    if !new {
      return;
    }
    // A `LoadThrough`'s key is dynamic: it registers under every name once it is known
    // (`load_through`).
    if let Constraint::Load { key: Key::Name(name), dst } = &self.vars[var].constraints[index] {
      let entry = (var, *dst);
      self.loads_by_name.entry(name.clone()).or_default().insert(entry);
    }
    self.new_constraints.push((var, index));
  }

  fn enqueue(&mut self, var: VarId) {
    if !self.vars[var].queued {
      self.vars[var].queued = true;
      self.worklist.push(var);
    }
  }

  /// The bytes of a function's body before it is generated: its `FnInfo` and the variables
  /// every body has.
  pub fn body_bytes(&self) -> usize {
    std::mem::size_of::<FnInfo>() + 5 * std::mem::size_of::<Var>()
  }

  pub fn activate(&mut self, f: FnId) {
    if self.fns[f].activated {
      return;
    }
    self.fns[f].activated = true;
    self.activation_queue.push(f);
  }

  // == solving

  /// Allocation budget: beyond it the analysis stops and keeps every method. Finite by
  /// construction, this is a last line of defence against a host running out of memory: the
  /// points-to and edge totals bound the memory the solver can take (about 16 bytes each).
  const MAX_OBJECTS: usize = 3_000_000;
  const MAX_VARS: usize = 30_000_000;
  const MAX_ENTRIES: u64 = 32_000_000;

  /// Checked on every turn of the solve loop, so the environment overrides are read once, in
  /// `new` (`EnvLimits`).
  /// Sets the variable cap, for tests that run out of variables at a chosen point.
  #[cfg(test)]
  pub(super) fn set_var_cap(&mut self, vars: usize) {
    self.env_limits.vars = vars;
  }

  pub(super) fn over_budget(&self) -> bool {
    self.objs.len() > self.env_limits.objects
      || self.vars.len() > self.env_limits.vars
      || self.entries() > self.env_limits.entries.unwrap_or(self.max_entries)
  }

  /// The entries the budget counts: points-to entries, copy edges, named-load rows and the
  /// loads replay queued.
  pub(super) fn entries(&self) -> u64 {
    self.total_pts + self.total_copies + self.total_named_loads + self.total_replay_loads
  }

  /// Sets the entry cap, for tests that run out of entries at a chosen point.
  #[cfg(test)]
  pub(super) fn set_entry_cap(&mut self, entries: u64) {
    self.env_limits.entries = Some(entries);
  }

  /// What the analysis holds, for tests counting what generation did.
  #[cfg(test)]
  pub(super) fn work(&self) -> Work {
    Work {
      vars: self.vars.len(),
      objs: self.objs.len(),
      entries: self.entries(),
      constraints: self.vars.iter().map(|v| v.constraints.len()).sum(),
      queued: self.new_constraints.len() + self.worklist.len() + self.activation_queue.len(),
      pending: self.fns.iter().map(|f| f.pending.len()).sum(),
      fns: self.fns.len(),
      locals: self.fns.iter().map(|f| f.locals.len()).sum(),
      slots: self
        .objs
        .iter()
        .map(|o| {
          o.fields.len()
            + usize::from(o.elem.is_some())
            + usize::from(o.named_elem.is_some())
            + usize::from(o.proto.is_some())
        })
        .sum(),
      records: self.method_names.len()
        + self.builtins.len()
        + self.inherits.len()
        + self.class_escaped.len()
        + self.hole_related.len()
        + self.prefixes.len()
        + self.prefix_hosts.len(),
      prefix_text: self.prefixes.values().chain(self.prefix_hosts.keys()).map(ArcStr::len).sum(),
    }
  }

  /// Counts of objects by kind (and builtins by rule), for diagnosing runaway analyses.
  pub fn histogram(&self) -> String {
    let mut counts: FxHashMap<String, usize> = FxHashMap::default();
    for o in &self.objs {
      let key = match &o.kind {
        ObjKind::Builtin { rule, .. } => format!("Builtin {}", self.rules[*rule as usize].0),
        other => format!("{other:?}").split([' ', '(', '{']).next().unwrap_or("").to_string(),
      };
      *counts.entry(key).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mut kinds: FxHashMap<String, usize> = FxHashMap::default();
    let mut total_constraints = 0usize;
    for var in &self.vars {
      total_constraints += var.constraints.len();
      for c in &var.constraints {
        *kinds
          .entry(format!("{c:?}").split([' ', '(', '{']).next().unwrap_or("").to_string())
          .or_default() += 1;
      }
    }
    let mut k: Vec<(String, usize)> = kinds.into_iter().collect();
    k.sort_by_key(|x| std::cmp::Reverse(x.1));
    // Loads by property name, calls by module, and the fattest variables.
    let mut by_name: FxHashMap<ArcStr, usize> = FxHashMap::default();
    let mut by_module: FxHashMap<u32, usize> = FxHashMap::default();
    for var in &self.vars {
      for c in &var.constraints {
        match c {
          Constraint::Load { key: Key::Name(n), .. } => *by_name.entry(n.clone()).or_default() += 1,
          Constraint::Call(call) => *by_module.entry(call.site.module.raw()).or_default() += 1,
          _ => {}
        }
      }
    }
    let mut names: Vec<(ArcStr, usize)> = by_name.into_iter().collect();
    names.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mut modules: Vec<(u32, usize)> = by_module.into_iter().collect();
    modules.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mut fat: Vec<(usize, usize)> =
      self.vars.iter_enumerated().map(|(v, var)| (var.pts.len(), v.index())).collect();
    fat.sort_by_key(|x| std::cmp::Reverse(x.0));
    let fat_desc: Vec<String> = fat
      .iter()
      .take(8)
      .map(|(n, v)| {
        let var = VarId::from_usize(*v);
        let mut kinds: FxHashMap<&'static str, usize> = FxHashMap::default();
        for o in &self.vars[var].pts {
          *kinds
            .entry(match &self.objs[*o].kind {
              ObjKind::Closure(_) => "closure",
              ObjKind::Object => "object",
              ObjKind::Array => "array",
              ObjKind::Effect { .. } => "effect",
              ObjKind::Str(_) => "str",
              ObjKind::Builtin { .. } => "builtin",
              _ => "other",
            })
            .or_default() += 1;
        }
        format!("v{v}:{n} {kinds:?} c{}", self.vars[var].constraints.len())
      })
      .collect();
    if self.debug {
      eprintln!(
        "[effect-service-shaking] loads by name {:?}; calls by module {:?}; fattest vars {:?}",
        &names[..names.len().min(15)],
        &modules[..modules.len().min(8)],
        fat_desc
      );
    }
    let mut calls: Vec<(&str, usize)> =
      self.rule_calls.iter().map(|(r, n)| (self.rules[*r as usize].0, *n)).collect();
    calls.sort_by_key(|x| std::cmp::Reverse(x.1));
    let pts: usize = self.vars.iter().map(|v| v.pts.len()).sum();
    let copies: usize = self.vars.iter().map(|v| v.copies.len()).sum();
    let fields: usize = self
      .objs
      .iter()
      .map(|o| {
        o.fields.len() + o.unknown_loads.len() + o.unknown_stores.len() + o.spread_from.len()
      })
      .sum();
    let rss_kb = std::fs::read_to_string("/proc/self/statm")
      .ok()
      .and_then(|s| s.split_whitespace().nth(1).and_then(|p| p.parse::<usize>().ok()))
      .map_or(0, |pages| pages * 4);
    format!(
      "rss {} MiB; pts {pts}, copies {copies}, field slots {fields}, memo {}, pending {}, queued {}; objects by kind: {:?}; vars {}, constraints {} by kind {:?}; rule calls {:?}",
      rss_kb / 1024,
      self.rule_memo.len(),
      self.new_constraints.len(),
      self.worklist.len(),
      &v[..v.len().min(12)],
      self.vars.len(),
      total_constraints,
      &k[..k.len().min(10)],
      &calls[..calls.len().min(12)]
    )
  }

  /// Solves with every body already generated.
  #[cfg(test)]
  pub fn solve(&mut self) {
    self.solve_with(&mut |_, _| unreachable!("a deferred body needs a generator (solve_with)"));
  }

  /// Solves to a fixpoint. `generate` fills a deferred body's `pending` when the body is first
  /// installed: its constraints, and the activations its references add. It runs between the
  /// solver's steps, never inside one. The generator checks the budget as it goes and stops
  /// part way through a statement when it runs out (`Gen::checkpoint`); the solve then stops
  /// before installing anything of it, terminally (every method kept, no retry).
  pub fn solve_with(&mut self, generate: &mut dyn FnMut(&mut Analysis, FnId)) {
    'solve: loop {
      if self.exhausted || self.replay_over || self.over_budget() {
        self.budget_exceeded = true;
        if self.debug {
          self.report_demand_at_stop();
        }
        self.bail_reasons.push(self.histogram());
        self.bail_everything("the analysis exceeded its allocation budget; every method is kept");
        self.worklist.clear();
        self.new_constraints.clear();
        self.activation_queue.clear();
        break;
      }
      while let Some(f) = self.activation_queue.pop() {
        if self.fns[f].deferred {
          self.fns[f].deferred = false;
          generate(self, f);
          // A body generated part way, or one that ran past the budget, is never installed: the
          // loop's start stops the solve.
          if self.exhausted || self.over_budget() {
            self.budget_exceeded_generating = true;
            continue 'solve;
          }
        }
        let pending = if self.fns[f].cloneable {
          self.fns[f].pending.clone()
        } else {
          std::mem::take(&mut self.fns[f].pending)
        };
        for p in pending {
          match p {
            Pending::Copy(src, dst) => self.copy(src, dst),
            Pending::Constraint(var, c) => self.constrain(var, c),
            Pending::Seed(var, obj) => self.seed(var, obj),
            Pending::Activate(inner) => self.activate(inner),
          }
        }
      }
      if let Some((var, index)) = self.new_constraints.pop() {
        let objs: Vec<ObjId> = self.vars[var].pts.iter().copied().collect();
        if !objs.is_empty() {
          let c = self.vars[var].constraints[index].clone();
          let before = self.steps;
          self.steps += objs.len() as u64;
          if self.debug && self.steps / 1_000_000 != before / 1_000_000 {
            eprintln!(
              "[effect-service-shaking] solving (constraints): {} pending, {} queued vars; {}",
              self.new_constraints.len(),
              self.worklist.len(),
              self.histogram()
            );
          }
          for obj in objs {
            self.apply(var, obj, &c);
          }
        }
        continue;
      }
      let Some(var) = self.worklist.pop() else {
        if self.activation_queue.is_empty() && self.new_constraints.is_empty() {
          // Rejected objects are re-checked as they change; this pass only confirms that
          // nothing became admissible without an event before the solve ends.
          if self.recheck_all() | self.recheck_deferred() | self.settle_admissions() {
            continue;
          }
          if self.release_admissions() {
            continue;
          }
          break;
        }
        continue;
      };
      self.vars[var].queued = false;
      let delta = std::mem::take(&mut self.vars[var].delta);
      if delta.is_empty() {
        continue;
      }
      self.steps += delta.len() as u64;
      if self.debug && self.steps / 1_000_000 != (self.steps - delta.len() as u64) / 1_000_000 {
        eprintln!("[effect-service-shaking] solving: {}", self.histogram());
      }
      let copies: Vec<VarId> = self.vars[var].copies.iter().copied().collect();
      for dst in copies {
        for obj in &delta {
          self.seed(dst, *obj);
        }
      }
      let n = self.vars[var].constraints.len();
      for i in 0..n {
        let c = self.vars[var].constraints[i].clone();
        for obj in &delta {
          self.apply(var, *obj, &c);
        }
      }
    }
  }

  fn apply(&mut self, var: VarId, obj: ObjId, c: &Constraint) {
    let saved = self.var_tag;
    self.var_tag = self.constraint_tag(c);
    self.apply_inner(var, obj, c);
    self.var_tag = saved;
  }

  /// Diagnostic: the constraint kind, as a tag for the variables its handler creates.
  fn constraint_tag(&mut self, c: &Constraint) -> &'static str {
    let d = std::mem::discriminant(c);
    if let Some(t) = self.tag_names.get(&d) {
      return t;
    }
    let name = format!("{c:?}");
    let name = name.split([' ', '(', '{']).next().unwrap_or("").to_string();
    let t: &'static str = Box::leak(name.into_boxed_str());
    self.tag_names.insert(d, t);
    t
  }

  #[expect(clippy::too_many_lines, reason = "one dispatch over every constraint kind")]
  fn apply_inner(&mut self, var: VarId, obj: ObjId, c: &Constraint) {
    if !self.stand_ins.is_empty() && self.stand_in_use(obj, c) {
      return;
    }
    match c {
      Constraint::Load { key, dst } => self.load(obj, key, *dst),
      Constraint::Store { key, src } => self.store(obj, key, *src),
      Constraint::LoadThrough { base, dst } => self.load_through(obj, *base, *dst),
      Constraint::StoreThrough { base, src } => self.store_through(var, obj, *base, *src),
      Constraint::Call(call) => self.call(obj, call),
      Constraint::SuccessOf(dst) => self.success_of(obj, *dst),
      Constraint::FailureOf(dst) => self.failure_of(obj, *dst),
      Constraint::KeyUse(use_) => self.key_use(obj, use_),
      Constraint::KeyNameUse(use_) => self.key_name_use(obj, use_),
      Constraint::ContentsLookup(dst) => {
        if matches!(self.objs[obj].kind, ObjKind::AllKeys) {
          self.copy(self.effect_key_values, *dst);
          self.copy(self.unknown_key_values, *dst);
        } else {
          self.key_use(obj, &KeyUse::Lookup(*dst));
        }
      }
      Constraint::KeyHeritage(class, use_) => self.key_heritage(obj, *class, use_),
      Constraint::SubclassHeritage(use_) => self.subclass_heritage(obj, use_),
      Constraint::ContextKeys(dst) => match self.objs[obj].kind {
        ObjKind::Context { keys } | ObjKind::Layer { keys } | ObjKind::Runtime { keys } => {
          self.copy(keys, *dst);
        }
        // A Context effect's code made holds effect's keys; one from unseen code, keys it made.
        ObjKind::Wrap(_) | ObjKind::Typed { .. } => self.copy(self.effect_key_strings, *dst),
        ObjKind::Top(_) if obj == self.top_effect => self.copy(self.effect_key_strings, *dst),
        ObjKind::Top(Flavour::Host) => self.seed(*dst, self.all_keys),
        ObjKind::Top(_) => self.seed(*dst, self.unseen_keys),
        ObjKind::Builtin { .. } if self.is_default(obj) => self.copy(self.effect_key_strings, *dst),
        // A module hole's value may be a container whose keys are the program's: the hole is
        // analysed in the next solve (`boundary_key`).
        ObjKind::External { .. } => self.boundary_key(obj),
        _ => {}
      },
      Constraint::ElemOf(dst) => self.elem_of(obj, *dst),
      Constraint::Contents(dst) => self.contents_of(obj, *dst),
      Constraint::Demand(name) => {
        if matches!(
          self.objs[obj].kind,
          ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. }
        ) {
          self.objs[obj].demand.add(name);
          let sources = self.objs[obj].spread_from.clone();
          for src in sources {
            self.constrain(src, Constraint::Demand(name.clone()));
          }
        }
      }
      Constraint::DemandAll | Constraint::Enumerate => self.enumerate(obj),
      Constraint::Callables(dst) => {
        if matches!(
          self.objs[obj].kind,
          ObjKind::Closure(_)
            | ObjKind::Class { .. }
            | ObjKind::Builtin { .. }
            | ObjKind::Typed { .. }
        ) {
          self.seed(*dst, obj);
        }
      }
      Constraint::AwaitOf(dst) => match self.objs[obj].kind {
        ObjKind::Promise { value } => self.copy(value, *dst),
        ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. } => {
          // The object itself (awaiting a non-thenable yields it); a hand-written thenable
          // bails (`adopt`). An object that can't reach a callable has no callable `then`, so
          // its adoption waits until it may (direction 2's admission lever).
          self.seed(*dst, obj);
          if self.reaches_code(obj) {
            self.adopt(obj, *dst);
          } else {
            self.deferred.push(Deferred::Adopt(obj, *dst));
          }
        }
        // Awaiting a type-modelled value yields what it holds.
        ObjKind::Typed { slot, .. } => {
          self.seed(*dst, obj);
          self.copy(slot, *dst);
        }
        // A summary may stand for a thenable: what it wraps is awaited too, once per summary
        // (`awaited_wraps`), and every await of the summary receives that.
        ObjKind::Wrap(inner) => {
          self.seed(*dst, obj);
          let awaited = match self.awaited_wraps.get(&obj) {
            Some(s) => *s,
            None => {
              let s = self.var();
              self.awaited_wraps.insert(obj, s);
              self.constrain(inner, Constraint::AwaitOf(s));
              s
            }
          };
          self.copy(awaited, *dst);
        }
        _ => self.seed(*dst, obj),
      },
      Constraint::Escape(flavour, site) => self.escape(obj, *flavour, *site),
      Constraint::ClassEscape(class, how) => self.class_escape(obj, *class, *how),
      c @ (Constraint::ClassRef(..) | Constraint::ClassReceiver(_)) => self.apply_class(obj, c),
      Constraint::HasInstance(x, site) => self.has_instance(obj, *x, *site),
      Constraint::Protocol(protocol) => {
        let bit = match protocol {
          Protocol::Json => PROTO_JSON,
          Protocol::Primitive => PROTO_PRIMITIVE,
          Protocol::Inspect => PROTO_INSPECT,
        };
        self.objs[obj].protocols |= bit;
        let _ = var;
        self.host_protocol(obj, *protocol);
      }
      Constraint::SpreadInto(target) => self.spread_into(obj, *target),
      Constraint::SpreadTarget(src) => {
        self.cross_stored(obj, *src);
        self.constrain(*src, Constraint::SpreadInto(obj));
      }
      Constraint::ProtoOf(dst) => match self.objs[obj].kind {
        ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. } => {
          let pv = self.proto_var(obj);
          self.copy(pv, *dst);
        }
        // A boundary's value inherits from that boundary: the opaque value from its class
        // (whatever crossed into it), any other from code the pass can't see.
        ObjKind::Top(_) | ObjKind::External { .. } => {
          let t = self.top_of(obj);
          self.seed(*dst, t);
        }
        _ => {}
      },
      Constraint::Provision => match self.objs[obj].kind {
        ObjKind::Builtin { .. } if self.is_default(obj) => {
          // A default-modelled effect layer provides effect-declared keys with values the pass
          // cannot see.
          let keys = self.effect_keys.clone();
          for k in keys {
            let v = self.key_value_var(&k);
            self.seed(v, self.top_host);
          }
        }
        ObjKind::Top(_) | ObjKind::Wrap(_) | ObjKind::Typed { .. } | ObjKind::External { .. } => {
          let t = self.top_of(obj);
          self.seed(self.unknown_key_values, t);
        }
        ObjKind::Array | ObjKind::Object => {
          // Members of an array or record of layers: provision each, through the object's own
          // variables so a cyclic container cannot create variables without end.
          let elem = self.elem(obj);
          self.constrain(elem, Constraint::Provision);
          let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
          for f in fields {
            self.constrain(f, Constraint::Provision);
          }
          let sources = self.objs[obj].spread_from.clone();
          for src in sources {
            self.constrain(src, Constraint::Provision);
          }
        }
        _ => {}
      },
      Constraint::MapKeys(dst) => match self.objs[obj].kind {
        ObjKind::Str(_) => self.seed(*dst, obj),
        ObjKind::Wrap(_) | ObjKind::Typed { .. } => self.copy(self.effect_key_strings, *dst),
        ObjKind::Top(_) if obj == self.top_effect => self.copy(self.effect_key_strings, *dst),
        ObjKind::External { .. } if self.is_opaque(obj) => self.boundary_key(obj),
        ObjKind::Top(Flavour::Host) | ObjKind::External { flavour: Flavour::Host, .. } => {
          self.seed(*dst, self.all_keys);
        }
        ObjKind::Top(_) | ObjKind::External { .. } => self.seed(*dst, self.unseen_keys),
        _ => {}
      },
      Constraint::Inherits(heir) => self.inherit(*heir, obj),
      Constraint::FilterCopy { dst, ty } => {
        if matches!(
          self.objs[obj].kind,
          ObjKind::Wrap(_) | ObjKind::Typed { .. } | ObjKind::Top(_) | ObjKind::External { .. }
        ) && !self.admits(*ty, obj)
        {
          // A summary may stand for a value the type admits, such as a scalar or a key: it
          // becomes an unknown scalar, which carries no service but stays any key. At an
          // ignored position it is a plain primitive: nothing of it is read.
          let t = if self.tys[*ty as usize].ignored { self.prim } else { self.top_host };
          self.seed(*dst, t);
        } else {
          self.filter(obj, *ty, *dst);
        }
      }
      Constraint::Walk { walk, pool, arg, site } => self.walk(obj, *walk, *pool, *arg, *site),
      Constraint::Witness(witness, dst) => self.seed(*dst, *witness),
      // A thenable's `then` runs whoever made it: the thenable goes to that code, and the await
      // settles with what it makes. Class code or a widened set (`opaque_class`) keeps it in its
      // boundary, effect's code (`top_effect`, a summary) in effect's, anything else is code the
      // pass can't see.
      Constraint::ThenableBail(thenable) => {
        if Self::callable_kind(&self.objs[obj].kind) && self.thenables_bailed.insert(*thenable) {
          if let Some(&class) = self.opaque_class.get(&obj) {
            self.class_escape(*thenable, class, CLASS_HANDED);
          } else if obj == self.top_effect || matches!(self.objs[obj].kind, ObjKind::Typed { .. }) {
            self.escape(*thenable, Flavour::Ext, Some(EFFECT_ESCAPE));
          } else if matches!(self.objs[obj].kind, ObjKind::Top(_) | ObjKind::External { .. }) {
            self.escape(*thenable, Flavour::Ext, None);
          } else {
            // A hand-written `then`: program code the pass does not follow.
            let class = self.unfollowed_class();
            self.class_escape(*thenable, class, CLASS_UNFOLLOWED);
          }
        }
      }
      Constraint::ThenableAwait(dst) => {
        if Self::callable_kind(&self.objs[obj].kind) {
          let t = if self.opaque_class.contains_key(&obj) {
            obj
          } else if obj == self.top_effect || matches!(self.objs[obj].kind, ObjKind::Typed { .. }) {
            self.top_effect
          } else if matches!(self.objs[obj].kind, ObjKind::Top(_) | ObjKind::External { .. }) {
            self.top_ext
          } else {
            self.unfollowed()
          };
          self.seed(*dst, t);
        }
      }
      Constraint::TypedArg(slot, back, site, mask, prim, opaque) => {
        self.typed_arg(obj, *slot, *back, *site, *mask, *prim, *opaque);
      }
      Constraint::TypedReceiver(callee, name, slot, back, site) => {
        if matches!(self.objs[obj].kind, ObjKind::Typed { .. }) {
          self.typed_arg(obj, *slot, *back, *site, 0, false, false);
        } else {
          let holder = self.holder(obj);
          let member = self.receiver_member(obj, name);
          // Already registered when the member is shared: the set dedupes it.
          self.constrain(holder, Constraint::Load { key: Key::Name(name.clone()), dst: member });
          self.constrain(member, Constraint::TypedReceiverIf(*callee, obj, *slot, *back, *site));
        }
      }
      Constraint::TypedReceiverIf(callee, recv, slot, back, site) => {
        if obj == *callee {
          self.typed_arg(*recv, *slot, *back, *site, 0, false, false);
        }
      }
      Constraint::SlotIn(dst, stand_in) => self.slot_in(obj, *dst, *stand_in),
      Constraint::AdmitTyped(dst, prim) => self.admit_typed_with(obj, *dst, !*prim),
      Constraint::TypedConcrete(ty, site) => self.typed_concrete(obj, *ty, *site),
      Constraint::CallFn(call) => self.call_fn_only(obj, call),
      Constraint::ProtocolCall(call) => self.protocol_method_call(obj, call),
      Constraint::EscapeOut => match self.objs[obj].kind {
        ObjKind::Effect { success, .. } => self.constrain(success, Constraint::EscapeOut),
        ObjKind::Promise { value } => self.constrain(value, Constraint::EscapeOut),
        _ => self.escape(obj, Flavour::Ext, Some(EFFECT_ESCAPE)),
      },
      Constraint::TypedNested(slot, back, site, mask) => {
        self.typed_nested(obj, *slot, *back, *site, *mask);
      }
      Constraint::EscapeHeld => self.escape_with(obj, Flavour::Ext, Some(EFFECT_ESCAPE), false),
      Constraint::BailKeys => self.bail_keys(obj),
      Constraint::HandName => self.hand_name(obj),
      Constraint::LookupIn { key, dst } => self.lookup_in(obj, *key, *dst),
      Constraint::LookupNameIn { key, dst } => {
        if matches!(
          self.objs[obj].kind,
          ObjKind::Context { .. }
            | ObjKind::Top(_)
            | ObjKind::External { .. }
            | ObjKind::Wrap(_)
            | ObjKind::Typed { .. }
        ) {
          self.constrain(*key, Constraint::KeyNameUse(KeyUse::Lookup(*dst)));
        }
      }
      Constraint::EscapeReceiver(flavour, site, name) => {
        self.escape_receiver(obj, *flavour, *site, name);
      }
      Constraint::EscapeIfOpaque(receiver, flavour, site) => {
        if matches!(self.objs[obj].kind, ObjKind::Top(_) | ObjKind::External { .. }) {
          self.escape(*receiver, *flavour, *site);
        }
      }
      Constraint::Accessor(acc) => self.accessor(obj, acc),
      Constraint::SetProto(src) => {
        if matches!(
          self.objs[obj].kind,
          ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. }
        ) {
          let pv = self.proto_var(obj);
          self.copy(*src, pv);
        }
        self.cross_stored(obj, *src);
      }
    }
  }

  /// What is set on a boundary's value, as its prototype or spread into it, crosses into the
  /// boundary as a value stored on it does (`store_name`).
  fn cross_stored(&mut self, obj: ObjId, src: VarId) {
    if let ObjKind::Top(flavour) | ObjKind::External { flavour, .. } = self.objs[obj].kind {
      match self.opaque_class.get(&obj) {
        Some(&class) => self.constrain(src, Constraint::ClassEscape(class, CLASS_STORED)),
        None => self.constrain(src, Constraint::Escape(flavour, None)),
      }
    }
  }

  /// The variable a `TypedReceiver` site loads `recv.name` into, for its own
  /// `TypedReceiverIf` check. A site on a wrapped value or a string shares it with the earlier
  /// sites on the same receiver and name: the load there reads no state that changes during the
  /// solve, so it subscribes a later site to exactly what a fresh load would. A wrap's load
  /// copies the wrapped values, demands `name` of them and adds the wrap itself; a string's
  /// seeds the interned `String.prototype` builtin or host data. Every other receiver gets a
  /// fresh variable: an object's load reads its prototype only if it has one by then, a
  /// namespace's activates a lazy export, an external's reads its fields as they are.
  ///
  /// A shared member equals a fresh one only while neither it nor the wrapped values have
  /// widened: a widened variable drops what it held, and a fresh member would hold only what
  /// remains. The two cases differ. When the member widens, the next site gets a fresh member
  /// that replaces it in the memo, and later sites share that one (while the wrapped values
  /// have not widened). Once the wrapped values have widened, which may be before the widening
  /// has reached the cached member, the memo is not read or written for that wrap again: every
  /// later site on it gets its own fresh member for the rest of the solve.
  fn receiver_member(&mut self, recv: ObjId, name: &ArcStr) -> VarId {
    #[cfg(test)]
    if self.fresh_receiver_members {
      return self.var();
    }
    let shareable = match self.objs[recv].kind {
      ObjKind::Str(_) => true,
      ObjKind::Wrap(inner) => !self.vars[inner].widened,
      _ => false,
    };
    if !shareable {
      return self.var();
    }
    if let Some(&member) = self.receiver_members.get(&(recv, name.clone())) {
      if !self.vars[member].widened {
        return member;
      }
    }
    let member = self.var();
    self.receiver_members.insert((recv, name.clone()), member);
    member
  }

  /// A use of a stand-in (`StandIn`): one that only reads what the values it holds hold, or
  /// hands them over, applies to them (`held_var`); a call goes through `stand_in_call`.
  /// Returns whether the use is fully handled.
  fn stand_in_use(&mut self, obj: ObjId, c: &Constraint) -> bool {
    let Some(s) = self.stand_ins.get(&obj) else { return false };
    let held_var = s.held_var;
    match c {
      Constraint::Walk { .. }
      | Constraint::Escape(..)
      | Constraint::Protocol(_)
      | Constraint::Enumerate
      | Constraint::DemandAll
      | Constraint::SpreadInto(_)
      | Constraint::Contents(_)
      | Constraint::TypedConcrete(..)
      | Constraint::TypedNested(..)
      | Constraint::TypedArg(..)
      | Constraint::AwaitOf(_)
      | Constraint::ThenableBail(_) => {
        self.constrain(held_var, c.clone());
        false
      }
      Constraint::TypedReceiver(_, _, slot, back, site) => {
        self.constrain(held_var, Constraint::TypedArg(*slot, *back, *site, 0, false, false));
        false
      }
      Constraint::Call(call) => {
        let call = (**call).clone();
        self.stand_in_call(obj, held_var, &call);
        true
      }
      _ => false,
    }
  }

  fn load(&mut self, obj: ObjId, key: &Key, dst: VarId) {
    match key {
      Key::Name(name) => self.load_name(obj, name, dst),
      Key::Unknown => self.load_unknown(obj, dst),
      Key::Var(key_var) => {
        // Re-evaluated when the key variable grows: the key objects drive the load.
        let holder = self.holder(obj);
        self.constrain(*key_var, Constraint::LoadThrough { base: holder, dst });
      }
    }
  }

  fn load_through(&mut self, key: ObjId, base: VarId, dst: VarId) {
    let bases: Vec<ObjId> = self.vars[base].pts.iter().copied().collect();
    match self.objs[key].kind.clone() {
      ObjKind::Str(name) => {
        self.loads_by_name.entry(name.clone()).or_default().insert((base, dst));
        for b in bases {
          self.load_name(b, &name, dst);
        }
      }
      _ => {
        for b in bases {
          self.load_unknown(b, dst);
        }
      }
    }
  }

  /// `obj[k] = v` for one value of `k`.
  fn store_through(&mut self, key_var: VarId, key: ObjId, base: VarId, src: VarId) {
    let bases: Vec<ObjId> = self.vars[base].pts.iter().copied().collect();
    match self.objs[key].kind.clone() {
      // A key computed from data never writes one of effect's members (`is_effect_member`):
      // only a key that is exactly that member (a constant naming it) does.
      ObjKind::Str(name) if is_effect_member(&name) && self.vars[key_var].pts.len() > 1 => {}
      ObjKind::Str(name) => {
        let saved = std::mem::replace(&mut self.var_tag, "storethrough named");
        for b in bases {
          self.store_name(b, &name, src);
        }
        self.var_tag = saved;
      }
      // A number indexes an array: its elements, not a name.
      ObjKind::Num(_) => {
        for b in bases {
          if matches!(self.objs[b].kind, ObjKind::Array)
            && self.objs[b].escaped.is_none()
            && !self.class_escaped.contains_key(&b)
            && !self.objs[b].builtin_proto
          {
            let elem = self.elem(b);
            self.copy(src, elem);
          } else {
            self.store_unknown(b, src);
          }
        }
      }
      _ => {
        let saved = std::mem::replace(&mut self.var_tag, "storethrough unknown");
        for b in bases {
          self.store_unknown(b, src);
        }
        self.var_tag = saved;
      }
    }
  }

  pub fn load_name(&mut self, obj: ObjId, name: &ArcStr, dst: VarId) {
    self.touch_namespace(obj, Some(name));
    match self.objs[obj].kind.clone() {
      ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. } => {
        self.objs[obj].demand.add(name);
        if let ObjKind::Closure(f) = self.objs[obj].kind {
          if name.as_str() == "prototype" {
            self.function_prototype(obj, f);
          }
        }
        if let ObjKind::Class { prototype, .. } = self.objs[obj].kind {
          if name.as_str() == "prototype" {
            self.copy(prototype, dst);
          }
        }
        if matches!(self.objs[obj].kind, ObjKind::Closure(_)) {
          if let Some(id) = function_prototype_rule(self, name) {
            let b = self.method(id, obj);
            self.seed(dst, b);
            return;
          }
        }
        // The rest reads the elements, spread sources and prototype as they are now: recorded,
        // so what the object gains later reaches this read too (`Obj::named_loads`). A row is an
        // entry; none is added once the analysis is over budget, which stops the solve.
        if !self.replay_stopped()
          && self.objs[obj].named_loads.get_or_insert_default().insert((name.clone(), dst))
        {
          self.total_named_loads += 1;
        }
        if matches!(self.objs[obj].kind, ObjKind::Array) {
          if let Some(id) = self.rule_id(&format!("Array.prototype.{name}")) {
            let b = self.method(id, obj);
            self.seed(dst, b);
          }
        }
        self.read_field(obj, name, dst);
        if is_effect_member(name) {
          // Only a store naming it writes one of effect's members (`is_effect_member`).
        } else if matches!(self.objs[obj].kind, ObjKind::Array) && !is_index(name) {
          // A named read on an array sees stores that may name it, not the elements.
          let named = self.named_elem(obj);
          self.copy(named, dst);
        } else if let Some(elem) = self.objs[obj].elem {
          self.copy(elem, dst);
        }
        let sources = self.objs[obj].spread_from.clone();
        for src in sources {
          self.constrain(src, Constraint::Load { key: Key::Name(name.clone()), dst });
        }
        if let Some(proto) = self.objs[obj].proto {
          self.constrain(proto, Constraint::Load { key: Key::Name(name.clone()), dst });
        }
      }
      ObjKind::Str(_) => {
        // String methods: `join` and friends are handled on arrays; strings yield host data.
        if let Some(id) = self.rule_id(&format!("String.prototype.{name}")) {
          let b = self.method(id, obj);
          self.seed(dst, b);
        } else {
          self.seed(dst, self.top_host);
        }
      }
      ObjKind::Wrap(inner) => {
        // Any access path yields the wrapped values; the read is recorded on them.
        self.copy(inner, dst);
        self.constrain(inner, Constraint::Demand(name.clone()));
        self.seed(dst, obj);
      }
      ObjKind::Typed { slot, .. } => self.typed_member(obj, slot, dst, Some(name)),
      ObjKind::Top(_) => {
        let t = self.top_of(obj);
        self.seed(dst, t);
      }
      ObjKind::External { flavour, name: ref module } => {
        if let Some(f) = self.objs[obj].fields.get(name).copied() {
          self.copy(f, dst);
          return;
        }
        let full = format!("{module}.{name}");
        if self.opaque_class.contains_key(&obj) {
          self.seed(dst, obj);
        } else if let Some(id) = self.rule_id(&full) {
          let b = self.builtin(id, None, None);
          self.seed(dst, b);
        } else {
          let t = if flavour == Flavour::Ext { self.top_ext } else { self.top_host };
          self.seed(dst, t);
        }
      }
      ObjKind::Namespace => {
        // Namespace member reads are resolved during constraint generation; a
        // dynamic read falls through to every export via `fields`/`elem`.
        self.read_field(obj, name, dst);
        if let Some(elem) = self.objs[obj].elem {
          self.copy(elem, dst);
        }
      }
      ObjKind::Builtin { .. } => {
        // A builtin is a function too: `.call`, `.apply` and `.bind` call it with the receiver
        // and arguments they are given, as they do a closure (`builtin.call(other, f)`).
        if let Some(id) = function_prototype_rule(self, name) {
          let b = self.method(id, obj);
          self.seed(dst, b);
          return;
        }
        self.read_field(obj, name, dst);
      }
      ObjKind::Key { .. }
      | ObjKind::Context { .. }
      | ObjKind::Layer { .. }
      | ObjKind::MemoMap
      | ObjKind::Effect { .. }
      | ObjKind::Fiber
      | ObjKind::Runtime { .. }
      | ObjKind::Promise { .. }
      | ObjKind::Iterator(_) => {
        let holder = self.holder(obj);
        // An Exit is an Effect that also has the public Exit members.
        let kind_name =
          if self.exits.contains(&obj) && self.rule_id(&format!("Exit.{name}:get")).is_some() {
            "Exit"
          } else {
            model_kind_name(&self.objs[obj].kind)
          };
        let full = format!("{kind_name}.{name}");
        if let Some(id) = self.rule_id(&full) {
          let b = self.method(id, obj);
          self.seed(dst, b);
        } else if let Some(id) = self.rule_id(&format!("{kind_name}.{name}:get")) {
          // A getter-like member: the rule computes the value into `dst`.
          let call = Call {
            site: Site { module: ModuleIdx::from_raw(0), start: 0, sub: 0 },
            this: Some(holder),
            args: vec![],
            ret: dst,
            is_new: false,
            builtin: None,
          };
          (self.rules[id as usize].1)(self, &call);
        } else if matches!(
          self.objs[obj].kind,
          ObjKind::Context { .. }
            | ObjKind::Layer { .. }
            | ObjKind::Runtime { .. }
            | ObjKind::MemoMap
        ) {
          // Effect's containers have only their public members (rules above). Any other member
          // does not type check, so it reads nothing and bails nothing (issue, "Assumptions" 2).
          // `mapUnsafe` and every MemoMap member are ignored too ("When a method is kept").
        } else {
          self.read_field(obj, name, dst);
        }
      }
      ObjKind::AllKeys => {}
      ObjKind::Prim | ObjKind::Num(_) => self.seed(dst, self.prim),
    }
  }

  fn load_unknown(&mut self, obj: ObjId, dst: VarId) {
    self.touch_namespace(obj, None);
    match self.objs[obj].kind.clone() {
      ObjKind::Object
      | ObjKind::Array
      | ObjKind::Closure(_)
      | ObjKind::Class { .. }
      | ObjKind::Namespace
      | ObjKind::Builtin { .. } => {
        if let ObjKind::Closure(f) = self.objs[obj].kind {
          self.function_prototype(obj, f);
        }
        self.objs[obj].demand.all();
        let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
        for f in fields {
          self.copy(f, dst);
        }
        if !self.objs[obj].unknown_loads.contains(&dst) {
          self.objs[obj].unknown_loads.push(dst);
        }
        let elem = self.elem(obj);
        self.copy(elem, dst);
        let sources = self.objs[obj].spread_from.clone();
        for src in sources {
          self.constrain(src, Constraint::Load { key: Key::Unknown, dst });
        }
        if let Some(proto) = self.objs[obj].proto {
          self.constrain(proto, Constraint::Load { key: Key::Unknown, dst });
        }
        if let ObjKind::Class { prototype, .. } = self.objs[obj].kind {
          self.copy(prototype, dst);
        }
      }
      ObjKind::Wrap(inner) => {
        self.copy(inner, dst);
        self.constrain(inner, Constraint::DemandAll);
        self.seed(dst, obj);
      }
      ObjKind::Typed { slot, .. } => self.typed_member(obj, slot, dst, None),
      ObjKind::Str(_) => self.seed(dst, self.top_host),
      ObjKind::Top(_) | ObjKind::External { .. } => {
        let t = self.top_of(obj);
        self.seed(dst, t);
      }
      // A container read by an unknown name: under the public types it can only be one of its
      // public members, none of which hands out a service.
      ObjKind::Context { .. }
      | ObjKind::Runtime { .. }
      | ObjKind::Layer { .. }
      | ObjKind::MemoMap => {}
      ObjKind::Fiber => {
        // `fiber[x]` may reach `context`: the ambient Context.
        self.seed(dst, self.ambient_context);
      }
      ObjKind::Key { .. }
      | ObjKind::Effect { .. }
      | ObjKind::Promise { .. }
      | ObjKind::Iterator(_)
      | ObjKind::AllKeys => {
        let field = self.elem(obj);
        self.copy(field, dst);
      }
      ObjKind::Prim | ObjKind::Num(_) => self.seed(dst, self.prim),
    }
  }

  fn store(&mut self, obj: ObjId, key: &Key, src: VarId) {
    match key {
      Key::Var(key_var) => {
        let holder = self.holder(obj);
        self.constrain(*key_var, Constraint::StoreThrough { base: holder, src });
      }
      Key::Name(name) => self.store_name(obj, name, src),
      Key::Unknown => self.store_unknown(obj, src),
    }
  }

  pub fn store_name(&mut self, obj: ObjId, name: &ArcStr, src: VarId) {
    if self.objs[obj].builtin_proto {
      self.bail_everything("a builtin prototype is modified: the Context model no longer holds");
    }
    match self.objs[obj].kind.clone() {
      ObjKind::Key { strings, .. } => {
        // `key.key = x` retargets the key: whatever `x` names is now looked up through it.
        if name.as_str() == "key" {
          self.copy(src, strings);
        } else {
          let elem = self.elem(obj);
          self.copy(src, elem);
        }
      }
      ObjKind::Object
      | ObjKind::Array
      | ObjKind::Closure(_)
      | ObjKind::Class { .. }
      | ObjKind::Namespace
      | ObjKind::Builtin { .. } => {
        if matches!(self.objs[obj].kind, ObjKind::Closure(_) | ObjKind::Class { .. })
          && name.as_str() == "prototype"
        {
          // `F.prototype = x`: instances created later read x. Approximate: x escapes nowhere, but
          // reads through `prototype` see it.
        }
        if let Some(Flavour::Ext) = self.objs[obj].escaped {
          let site = (!self.genuine_escaped.contains(&obj)).then_some(EFFECT_ESCAPE);
          self.constrain(src, Constraint::Escape(Flavour::Ext, site));
        }
        for class in self.crossed_classes(obj) {
          self.constrain(src, Constraint::ClassEscape(class, CLASS_STORED));
        }
        let field = self.field(obj, name);
        self.copy(src, field);
      }
      ObjKind::Top(flavour) | ObjKind::External { flavour, .. } => {
        match self.opaque_class.get(&obj) {
          Some(&class) => self.constrain(src, Constraint::ClassEscape(class, CLASS_STORED)),
          None => self.constrain(src, Constraint::Escape(flavour, None)),
        }
      }
      ObjKind::Typed { .. } if self.is_stand_in(obj) => {
        self.taint_by(obj, &format!("store {name}"));
      }
      // A value written onto a summary joins what it holds through the admission filter.
      ObjKind::Typed { slot: inner, .. } => {
        self.constrain(src, Constraint::AdmitTyped(inner, false));
      }
      ObjKind::Wrap(inner) => self.copy(src, inner),
      // The only writable public member of a container is `ManagedRuntime.cachedContext`: the
      // Context written there provides its services. Other writes don't type check, so they
      // store nothing.
      ObjKind::Runtime { .. } if name.as_str() == "cachedContext" => {
        self.constrain(src, Constraint::Provision);
      }
      ObjKind::Context { .. }
      | ObjKind::Layer { .. }
      | ObjKind::Runtime { .. }
      | ObjKind::MemoMap
      | ObjKind::Prim
      | ObjKind::Num(_) => {}
      _ => {
        let elem = self.elem(obj);
        self.copy(src, elem);
      }
    }
  }

  fn store_unknown(&mut self, obj: ObjId, src: VarId) {
    if self.objs[obj].builtin_proto {
      self.bail_everything("a builtin prototype is modified: the Context model no longer holds");
    }
    match self.objs[obj].kind.clone() {
      ObjKind::Key { strings, .. } => self.copy(src, strings),
      ObjKind::Object
      | ObjKind::Array
      | ObjKind::Closure(_)
      | ObjKind::Class { .. }
      | ObjKind::Namespace
      | ObjKind::Builtin { .. } => {
        if let Some(Flavour::Ext) = self.objs[obj].escaped {
          let site = (!self.genuine_escaped.contains(&obj)).then_some(EFFECT_ESCAPE);
          self.constrain(src, Constraint::Escape(Flavour::Ext, site));
        }
        for class in self.crossed_classes(obj) {
          self.constrain(src, Constraint::ClassEscape(class, CLASS_STORED));
        }
        // Direction 2's admission lever: a value stored under an unknown name reaches every
        // field, so only what may hold a service flows as itself; the rest is stood for.
        let src = self.admitted_values(src);
        let elem = self.elem(obj);
        self.copy(src, elem);
        if matches!(self.objs[obj].kind, ObjKind::Array) {
          let named = self.named_elem(obj);
          self.copy(src, named);
        }
        if !self.objs[obj].unknown_stores.contains(&src) {
          self.objs[obj].unknown_stores.push(src);
          self.graph_changed();
          self.recheck_tree(obj);
        }
        let fields: Vec<VarId> = self.objs[obj]
          .fields
          .iter()
          .filter(|(n, _)| !is_effect_member(n))
          .map(|(_, v)| *v)
          .collect();
        for f in fields {
          self.copy(src, f);
        }
      }
      ObjKind::Top(flavour) | ObjKind::External { flavour, .. } => {
        match self.opaque_class.get(&obj) {
          Some(&class) => self.constrain(src, Constraint::ClassEscape(class, CLASS_STORED)),
          None => self.constrain(src, Constraint::Escape(flavour, None)),
        }
      }
      ObjKind::Typed { .. } if self.is_stand_in(obj) => self.taint_by(obj, "unknown store"),
      // A value written onto a summary joins what it holds through the admission filter.
      ObjKind::Typed { slot: inner, .. } => {
        self.constrain(src, Constraint::AdmitTyped(inner, false));
      }
      ObjKind::Wrap(inner) => self.copy(src, inner),
      ObjKind::Context { .. }
      | ObjKind::Layer { .. }
      | ObjKind::Runtime { .. }
      | ObjKind::MemoMap => {}
      _ => {
        let elem = self.elem(obj);
        self.copy(src, elem);
      }
    }
  }

  fn call(&mut self, callee: ObjId, call: &Call) {
    let class = self.opaque_class.get(&callee).copied().or_else(|| {
      matches!(self.objs[callee].kind, ObjKind::Class { .. })
        .then(|| self.opaque_of.contains_key(&callee).then_some(callee))
        .flatten()
    });
    if let Some(class) = class {
      // A hole class or what its code made: the call runs class code with its arguments and
      // `this`, and yields the class's opaque value.
      for a in &call.args {
        let (Arg::Var(v) | Arg::Spread(v)) = a;
        self.constrain(*v, Constraint::ClassEscape(class, CLASS_HANDED));
      }
      if let Some(this) = call.this {
        self.constrain(this, Constraint::ClassEscape(class, CLASS_HANDED));
      }
      let op = self.opaque(class);
      self.seed(call.ret, op);
      return;
    }
    match self.objs[callee].kind.clone() {
      ObjKind::Closure(f) if call.is_new && call.this.is_none() => {
        // `new F(...)` on a function constructor: an instance per function, whose prototype is
        // `F.prototype`, and the body writes to it through `this`. An object the body returns
        // replaces the instance, so the result also takes the body's return values.
        let instance = self.site_obj(
          Site { module: self.fns[f].module, start: callee.raw(), sub: 0 },
          201,
          |_| ObjKind::Object,
        );
        let pv = self.proto_var(instance);
        let prototype = self.function_prototype(callee, f);
        self.copy(prototype, pv);
        self.call_fn(f, call, Some(instance));
        let ret = self.fns[f].ret;
        self.copy(ret, call.ret);
        self.seed(call.ret, instance);
      }
      ObjKind::Closure(f) => {
        self.call_fn(f, call, None);
      }
      ObjKind::Class { ctor, heritage, .. } if call.is_new && call.this.is_some() => {
        // A parent constructor run on the instance a derived class is building, by `super(...)`
        // or by a derived class without a constructor: it writes to that instance, not to an
        // instance of its own. Its result is what `this` becomes in the caller.
        self.construct_on(ctor, heritage, call, None);
      }
      ObjKind::Class { ctor, heritage, prototype } => {
        // `new C(...)` (or a bare call, treated the same): an instance per class.
        let instance = self.site_obj(
          Site { module: self.fns_module(ctor), start: callee.raw(), sub: 0 },
          200,
          |_| ObjKind::Object,
        );
        let pv = self.proto_var(instance);
        self.copy(prototype, pv);
        let this = self.holder(instance);
        let on_instance = Call { this: Some(this), ..call.clone() };
        self.construct_on(ctor, heritage, &on_instance, Some(instance));
        self.seed(call.ret, instance);
      }
      ObjKind::Builtin { rule, data, this: bound, .. } => {
        let this = bound.or(data).or(call.this);
        if let Some(first) = self.rule_memo.get(&(call.site, callee)).cloned() {
          // Merge into the first invocation at this site.
          if let (Some(t), Some(ft)) = (this, first.this) {
            self.copy(t, ft);
          }
          for (i, a) in call.args.iter().enumerate() {
            let v = match a {
              Arg::Var(v) | Arg::Spread(v) => *v,
            };
            match first.args.get(i) {
              Some(Arg::Var(fv) | Arg::Spread(fv)) => self.copy(v, *fv),
              None => {
                if let Some(Arg::Var(fv) | Arg::Spread(fv)) = first.args.last() {
                  self.copy(v, *fv);
                }
              }
            }
          }
          self.copy(first.ret, call.ret);
          return;
        }
        *self.rule_calls.entry(rule).or_default() += 1;
        let r = self.rules[rule as usize].1;
        let saved_tag = self.var_tag;
        self.var_tag = self.rules[rule as usize].0;
        // Fresh variables per position: later calls merge into them, never into the callers'
        // own variables (which may be shared literals).
        let this = this.map(|t| {
          let m = self.var();
          self.copy(t, m);
          m
        });
        let args: Vec<Arg> = call
          .args
          .iter()
          .map(|a| match a {
            Arg::Var(v) => {
              let m = self.var();
              self.copy(*v, m);
              Arg::Var(m)
            }
            Arg::Spread(v) => {
              let m = self.var();
              self.copy(*v, m);
              Arg::Spread(m)
            }
          })
          .collect();
        let merged = Call {
          site: call.site,
          this,
          args,
          ret: call.ret,
          is_new: call.is_new,
          builtin: Some(callee),
        };
        self.rule_memo.insert((call.site, callee), merged.clone());
        r(self, &merged);
        self.var_tag = saved_tag;
      }
      // A function effect's code made runs effect's code: what it is handed goes there. As for
      // any unseen method (`escape_args`), `o.name(...)` runs it with `this` only for the
      // receivers whose `name` it may be.
      ObjKind::Top(_) if callee == self.top_effect => {
        for a in &call.args {
          let (Arg::Var(v) | Arg::Spread(v)) = a;
          self.constrain(*v, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
        }
        if let Some(this) = call.this {
          let c = match self.method_names.get(&call.site) {
            Some(name) => {
              Constraint::EscapeReceiver(Flavour::Ext, Some(EFFECT_ESCAPE), name.clone())
            }
            None => Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)),
          };
          self.constrain(this, c);
        }
        self.seed(call.ret, callee);
      }
      ObjKind::Top(flavour) | ObjKind::External { flavour, .. } => {
        self.escape_args(call, flavour);
        let t = self.top_of(callee);
        self.seed(call.ret, t);
      }
      ObjKind::Wrap(inner) => {
        // Calling a wrapped value: arguments join the wrapped values, callbacks among them run
        // with those values (through the wrap's single callback variable).
        let cb = self.wrap_callbacks[&callee];
        for a in &call.args {
          let v = match a {
            Arg::Var(v) | Arg::Spread(v) => *v,
          };
          self.copy(v, inner);
          self.copy(v, cb);
        }
        if let Some(this) = call.this {
          self.copy(this, inner);
        }
        self.seed(call.ret, callee);
      }
      ObjKind::Key { .. } => {
        // `K(...)` is not a call form Effect supports; treat as opaque host behaviour.
        self.escape_args(call, Flavour::Host);
      }
      ObjKind::Typed { slot, export } => self.typed_call(callee, slot, export, call),
      _ => {}
    }
  }

  fn fns_module(&self, ctor: Option<FnId>) -> ModuleIdx {
    ctor.map_or(ModuleIdx::from_raw(0), |f| self.fns[f].module)
  }

  /// The array a function's rest parameter holds. One per function: the parameter itself is
  /// shared by every caller, so an array per call site would add objects and no precision (and
  /// rules mint call sites as they run, which made the count unbounded in practice).
  fn rest_array(&mut self, f: FnId) -> ObjId {
    if let Some(arr) = self.rest_arrays.get(&f) {
      return *arr;
    }
    let arr = self.alloc(ObjKind::Array, None);
    self.rest_arrays.insert(f, arr);
    arr
  }

  /// Clones beyond this depth (a clone called from a clone ...) are keyed by the template
  /// call site, which bounds the number of clones.
  const MAX_CLONE_DEPTH: u8 = 2;
  const MAX_CLONES_PER_FN: u32 = 64;

  /// Per-call-site cloning of small functions (design §2): a leaf function called from a
  /// known site gets its own copy of the body, so the values of different callers do not
  /// merge in its parameters. Its calls get clone-tagged sites, so the clones they make and
  /// the model objects rules allocate are per clone too. Variables the body does not own
  /// (captured bindings, object fields) stay shared, so a clone is never less sound than the
  /// shared body.
  fn clone_for(&mut self, f: FnId, call: &Call) -> FnId {
    if !self.fns[f].cloneable {
      return f;
    }
    let (origin, depth) = if call.site.sub & CLONE_SUB == CLONE_SUB {
      self.clone_sites[(call.site.sub & !CLONE_SUB) as usize]
    } else {
      (call.site, 0)
    };
    let key_site = if self.wrap_sites.contains(&call.site) {
      // Closures handed to wraps share one clone per function: a clone per wrap repeats the
      // same work for every wrap of a widely used callback and exhausts the budget.
      WRAP_CLONE_SITE
    } else if depth >= Self::MAX_CLONE_DEPTH {
      origin
    } else {
      call.site
    };
    if let Some(c) = self.clone_memo.get(&(f, key_site)) {
      return *c;
    }
    if self.fns[f].clones >= Self::MAX_CLONES_PER_FN {
      return f;
    }
    self.fns[f].clones += 1;
    let depth = (depth + 1).min(Self::MAX_CLONE_DEPTH);
    let info = &self.fns[f];
    let own = [info.this, info.ret, info.yielded_fail, info.yielded, info.arguments_elem];
    let locals = info.locals.clone();
    let template = info.pending.clone();
    let (params, rest, kind, returns_closure_only, closure, module, span_start, enclosing_this) = (
      info.params.clone(),
      info.rest,
      info.kind,
      info.returns_closure_only,
      info.closure,
      info.module,
      info.span_start,
      info.enclosing_this,
    );
    let mut map: FxHashMap<VarId, VarId> = FxHashMap::default();
    let tag = match self.clone_tags.get(&(module, span_start)) {
      Some(t) => *t,
      None => {
        let t: &'static str =
          Box::leak(format!("clone fn({}, {span_start})", module.raw()).into_boxed_str());
        self.clone_tags.insert((module, span_start), t);
        t
      }
    };
    let saved_tag = std::mem::replace(&mut self.var_tag, tag);
    for v in own.iter().chain(&locals).chain(&params).chain(rest.iter()) {
      if !map.contains_key(v) {
        let nv = self.var();
        map.insert(*v, nv);
      }
    }
    self.var_tag = saved_tag;
    let m = |v: VarId| *map.get(&v).unwrap_or(&v);
    let id = self.fns.next_idx();
    let mut tag = |site: Site| -> Site {
      let next = u32::try_from(self.clone_sites.len()).expect("too many clone sites");
      let index = *self.clone_site_ids.entry((site, id)).or_insert(next);
      if index == next {
        self.clone_sites.push((site, depth));
      }
      Site { module: site.module, start: site.start, sub: CLONE_SUB | index }
    };
    let mut pending: Vec<Pending> = template
      .iter()
      .map(|p| match p {
        Pending::Copy(a, b) => Pending::Copy(m(*a), m(*b)),
        Pending::Constraint(v, c) => Pending::Constraint(m(*v), c.remap(&m, &mut tag)),
        Pending::Seed(v, o) => Pending::Seed(m(*v), *o),
        Pending::Activate(g) => Pending::Activate(*g),
      })
      .collect();
    if let Some(e) = enclosing_this {
      pending.push(Pending::Copy(e, m(own[0])));
    }
    let mut clone = FnInfo::new(module, span_start, kind, own.map(m));
    clone.params = params.iter().map(|p| m(*p)).collect();
    clone.rest = rest.map(m);
    clone.returns_closure_only = returns_closure_only;
    clone.closure = closure;
    clone.pending = pending;
    let id2 = self.fns.push(clone);
    debug_assert_eq!(id, id2);
    self.clone_memo.insert((f, key_site), id);
    id
  }

  /// Runs a class's construction on `call.this`: its constructor, or for a derived class
  /// without one, its parent's construction with the same arguments (as JavaScript's implicit
  /// `constructor(...args) { super(...args) }` does). `call.ret` receives what the construction
  /// yields besides the instance: an object a constructor returns replaces the instance, and in
  /// a derived constructor `this` is whatever `super(...)` yielded (`generate.rs` copies
  /// `super`'s result into `this`). `instance` is the object `new` allocated, if this is the
  /// outermost construction.
  fn construct_on(
    &mut self,
    ctor: Option<FnId>,
    heritage: Option<VarId>,
    call: &Call,
    instance: Option<ObjId>,
  ) {
    match (ctor, heritage) {
      (Some(f), _) => {
        let f = self.call_fn(f, call, instance);
        let (ret, this) = (self.fns[f].ret, self.fns[f].this);
        self.copy(ret, call.ret);
        if heritage.is_some() {
          self.copy(this, call.ret);
        }
      }
      (None, Some(h)) => {
        let parent_call = Call {
          site: call.site,
          this: call.this,
          args: call.args.clone(),
          ret: call.ret,
          is_new: true,
          builtin: None,
        };
        self.constrain(h, Constraint::Call(Arc::new(parent_call)));
      }
      (None, None) => {}
    }
  }

  /// `F.prototype` of an ordinary function: a default object, whose `constructor` is `F`, until
  /// code replaces it, as JavaScript creates one with every function that can be a constructor.
  pub fn function_prototype(&mut self, closure: ObjId, f: FnId) -> VarId {
    let field = self.field(closure, &ArcStr::from("prototype"));
    if matches!(self.fns[f].kind, FnKind::Normal | FnKind::Generator)
      && self.default_prototypes.insert(closure)
    {
      let proto = self.site_obj(
        Site { module: self.fns[f].module, start: closure.raw(), sub: 0 },
        204,
        |_| ObjKind::Object,
      );
      self.seed(field, proto);
      // `F.prototype.constructor` is `F`.
      let constructor = self.field(proto, &ArcStr::from("constructor"));
      self.seed(constructor, closure);
    }
    field
  }

  /// Calls `f`, or its clone for this call site, and returns the function that ran.
  /// Debug: what each key's provided objects demand when the budget stops the solve, before
  /// everything is kept. This is the state of an unfinished solve, not a result.
  fn report_demand_at_stop(&self) {
    let mut keys: Vec<(&ArcStr, &VarId)> = self.key_values.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0));
    for (key, var) in keys {
      for o in &self.vars[*var].pts {
        if !matches!(self.objs[*o].kind, ObjKind::Object) {
          continue;
        }
        let demand = match &self.objs[*o].demand {
          Demand::None => String::new(),
          Demand::All => "all".to_string(),
          Demand::Names(names) => {
            let mut v: Vec<&str> = names.iter().map(ArcStr::as_str).collect();
            v.sort_unstable();
            v.join(",")
          }
        };
        eprintln!(
          "[effect-service-shaking] at the budget stop: key {key} object {} has {} fields, demand {demand}",
          o.index(),
          self.objs[*o].fields.len()
        );
      }
    }
  }

  /// How many exported functions had their type bounds resolved.
  pub fn bounded_functions(&self) -> usize {
    self.fn_bounds.len()
  }

  /// The type bounds of `f` (or the function it clones), resolving them on first use.
  fn bounds_of(&mut self, f: FnId) -> Option<FnBounds> {
    let key = (self.fns[f].module, self.fns[f].span_start);
    if let Some((path, name, nparams, returned)) = self.lazy_bounds.remove(&key) {
      let b = if returned {
        self.dts.returned_bounds(&path, &name, nparams)
      } else {
        self.dts.bounds(&path, &name, nparams)
      };
      let mut intern = |t: Option<super::dts::Ty>| {
        t.map(|t| {
          self.tys.push(t);
          u32::try_from(self.tys.len() - 1).expect("too many declared types")
        })
      };
      let params = b.params.into_iter().map(&mut intern).collect();
      let ret = intern(b.ret);
      let rest = intern(b.rest);
      self.fn_bounds.insert(key, FnBounds { params, ret, rest });
    }
    self.fn_bounds.get(&key).cloned()
  }

  /// `src` flows into `dst`, filtered by the declared type `ty` if there is one. What reaches an
  /// ignored (`unknown`/`any`) position is also walked for its protocol methods.
  fn bounded_copy(&mut self, src: VarId, dst: VarId, ty: Option<u32>) {
    match ty {
      Some(ty) => {
        self.constrain(src, Constraint::FilterCopy { dst, ty });
        if self.tys[ty as usize].ignored {
          let pool = self.ignored_pool();
          self.copy(src, pool);
        }
      }
      None => self.copy(src, dst),
    }
  }

  /// `bounded_copy` for a rule: `src` into `dst` filtered by `ty`, or `open` into `dst` when the
  /// position is open.
  pub fn bounded_copy_pub(&mut self, src: VarId, dst: VarId, ty: Option<u32>, open: VarId) {
    match ty {
      Some(_) => self.bounded_copy(src, dst, ty),
      None => self.copy(open, dst),
    }
  }

  /// Interns a declared type for `Constraint::FilterCopy`.
  pub fn intern_ty(&mut self, t: super::dts::Ty) -> u32 {
    self.tys.push(t);
    u32::try_from(self.tys.len() - 1).expect("too many declared types")
  }

  /// The program-wide pool of what reaches an `unknown`/`any` position of Effect's public API
  /// (Tim's decision on assumption 2): it is walked for its protocol methods (`Walk::Protocol`),
  /// and nothing else of it is read.
  pub fn ignored_pool(&mut self) -> VarId {
    if let Some(pool) = self.ignored_pool {
      return pool;
    }
    let pool = self.var();
    self.ignored_pool = Some(pool);
    let context = self.var();
    self.seed(context, self.ambient_context);
    self.constrain(
      pool,
      Constraint::Walk { walk: Walk::Protocol, pool, arg: context, site: protocol_site(4) },
    );
    pool
  }

  /// `[util.inspect.custom](depth, options, inspect)`'s arguments, as `Walk::Inspect` passes
  /// them: a primitive, host data and the `util.inspect` builtin.
  fn inspect_args(&mut self) -> VarId {
    if let Some(v) = self.inspect_args {
      return v;
    }
    let v = self.var();
    self.seed(v, self.prim);
    self.seed(v, self.top_host);
    if let Some(id) = self.rule_id("util.inspect") {
      let inspect = self.builtin(id, None, None);
      self.seed(v, inspect);
    }
    self.inspect_args = Some(v);
    v
  }

  /// Host adoption of an object an `await`, an async function's return or a promise resolution
  /// receives. A native Promise is followed exactly (`AwaitOf`); an object with a callable `then`
  /// is a hand-written thenable, which bails (Tim's decision): every service it can reach (its
  /// fields, what its `then` captures and what that passes to `resolve`/`reject`, nested
  /// thenables included) keeps every method, by escaping the object to code the pass can't see,
  /// and the await yields an unknown value. An object with no callable `then` yields itself.
  fn adopt(&mut self, obj: ObjId, dst: VarId) {
    let then = match self.thenable_then.get(&obj) {
      Some(t) => *t,
      None => {
        let this = self.holder(obj);
        let then = self.var();
        self.constrain(this, Constraint::Load { key: Key::Name(ArcStr::from("then")), dst: then });
        self.constrain(then, Constraint::ThenableBail(obj));
        self.thenable_then.insert(obj, then);
        then
      }
    };
    self.constrain(then, Constraint::ThenableAwait(dst));
  }

  /// An export of a type-modelled `effect` module (design section 11): its body is not
  /// analysed; calls take their flow from its `.d.ts` signature (`typed_call`).
  pub fn typed_export(
    &mut self,
    dts: std::path::PathBuf,
    name: ArcStr,
    module: ModuleIdx,
  ) -> ObjId {
    let export = u32::try_from(self.typed_exports.len()).expect("too many exports");
    self.typed_exports.push(TypedExport { dts, name, module });
    let slot = self.var();
    self.alloc(ObjKind::Typed { slot, export }, None)
  }

  /// The signature of a call with `nargs` arguments of a value an export call returned (a
  /// data-last form), read from the export's declared return; its container shape is the
  /// export's (`Dts::container_shape` counts the returned function's positions).
  fn returned_sig(&mut self, origin: (u32, usize), nargs: usize) -> Option<TypedSig> {
    if let Some(sig) = self.returned_sigs.get(&(origin, nargs)) {
      return sig.clone();
    }
    let outer = self.typed_sig(origin.0, origin.1);
    let e = &self.typed_exports[origin.0 as usize];
    let sig = match outer {
      Some(outer) => {
        let (dts, name) = (e.dts.clone(), e.name.clone());
        self.dts.returned_call(&dts, &name, origin.1, nargs).map(
          |(b, ret, (generic, rest_generic), masks, free_returns, opaque)| {
            let params = b.params.into_iter().map(|t| t.map(|t| self.intern_ty(t))).collect();
            let rest = b.rest.map(|t| self.intern_ty(t));
            TypedSig {
              params,
              rest,
              ret,
              stores: outer.stores,
              reads: outer.reads,
              generic,
              rest_generic,
              masks,
              free_returns,
              opaque,
            }
          },
        )
      }
      None => None,
    };
    self.returned_sigs.insert((origin, nargs), sig.clone());
    sig
  }

  fn typed_sig(&mut self, export: u32, nargs: usize) -> Option<TypedSig> {
    if export == NO_EXPORT {
      return None;
    }
    if let Some(sig) = self.typed_sigs.get(&(export, nargs)) {
      return Some(sig.clone());
    }
    let e = &self.typed_exports[export as usize];
    let (dts, name) = (e.dts.clone(), e.name.clone());
    let b = self.dts.bounds(&dts, &name, nargs);
    let ret = self.dts.ret_shape(&dts, &name, nargs);
    let (stores, reads) = self.dts.container_shape(&dts, &name, nargs);
    let (generic, rest_generic) =
      self.dts.generic_positions(&dts, &name, nargs).unwrap_or((vec![true; nargs], true));
    let masks = self.dts.callback_masks(&dts, &name, nargs).unwrap_or_else(|| vec![0xFF; nargs]);
    let free_returns =
      self.dts.callback_free_returns(&dts, &name, nargs).unwrap_or_else(|| vec![false; nargs]);
    let opaque =
      self.dts.opaque_positions(&dts, &name, nargs).unwrap_or_else(|| vec![false; nargs]);
    let params = b.params.into_iter().map(|t| t.map(|t| self.intern_ty(t))).collect();
    let rest = b.rest.map(|t| self.intern_ty(t));
    let sig = TypedSig {
      params,
      rest,
      ret,
      stores,
      reads,
      generic,
      rest_generic,
      masks,
      free_returns,
      opaque,
    };
    self.typed_sigs.insert((export, nargs), sig.clone());
    Some(sig)
  }

  /// The elements of a spread argument, one variable per spread source.
  fn typed_spread_elems(&mut self, v: VarId) -> VarId {
    if let Some(e) = self.typed_spreads.get(&v) {
      return *e;
    }
    let e = self.var();
    self.typed_spreads.insert(v, e);
    self.constrain(v, Constraint::ElemOf(e));
    e
  }

  /// `v` filtered by the declared type `ty` (`bounded_copy`), one variable per pair.
  fn typed_filtered_arg(&mut self, v: VarId, ty: u32) -> VarId {
    if let Some(a) = self.typed_filtered.get(&(v, ty)) {
      return *a;
    }
    let a = self.var();
    self.typed_filtered.insert((v, ty), a);
    self.bounded_copy(v, a, Some(ty));
    a
  }

  /// The module a type-modelled value comes from: an export's, or the producing call's.
  fn typed_module(&self, obj: ObjId) -> Option<ModuleIdx> {
    match self.objs[obj].kind {
      ObjKind::Typed { export, .. } if export != NO_EXPORT => {
        Some(self.typed_exports[export as usize].module)
      }
      _ => self.typed_result_module.get(&obj).copied(),
    }
  }

  /// A member read off a type-modelled value: what it holds, and a method summary that shares
  /// its slot (a method or a getter may hand out any of it). `pipe` is `Pipeable`'s, which every
  /// one of Effect's data types declares: the `effect:Pipeable.pipe` builtin, called with the
  /// value as `this`. A stand-in's members are values it stands for: itself.
  fn typed_member(&mut self, obj: ObjId, slot: VarId, dst: VarId, name: Option<&ArcStr>) {
    if name.is_some_and(|n| n.as_str() == "pipe") && !self.is_stand_in(obj) {
      if let Some(id) = self.rule_id("effect:Pipeable.pipe") {
        let pipe = self.builtin(id, None, None);
        self.seed(dst, pipe);
        return;
      }
    }
    // A name the value's module declares only as a method holds no value of a type parameter.
    let method = match (name, self.typed_module(obj)) {
      (Some(n), Some(m)) => {
        let names = match self.method_only.get(&m) {
          Some(s) => std::sync::Arc::clone(s),
          None => {
            let path = self.typed_exports.iter().find(|e| e.module == m).map(|e| e.dts.clone());
            let set =
              std::sync::Arc::new(path.map(|p| self.dts.method_only_names(&p)).unwrap_or_default());
            self.method_only.insert(m, std::sync::Arc::clone(&set));
            set
          }
        };
        names.contains(n.as_str())
      }
      _ => false,
    };
    if !method {
      self.copy(slot, dst);
    }
    // A value's method is the value itself (calling it is a call on it); an export's is a
    // separate summary, so that its own signature is not applied to a method call.
    if matches!(self.objs[obj].kind, ObjKind::Typed { export: NO_EXPORT, .. }) {
      self.seed(dst, obj);
      return;
    }
    let method = match self.typed_methods.get(&obj) {
      Some(m) => *m,
      None => {
        let m = self.alloc(ObjKind::Typed { slot, export: NO_EXPORT }, None);
        self.typed_methods.insert(obj, m);
        if let Some(module) = self.typed_module(obj) {
          self.typed_result_module.insert(m, module);
        }
        m
      }
    };
    self.seed(dst, method);
  }

  /// The one result a type-modelled call site returns at a concrete type: a summary that holds
  /// nothing the caller handed it.
  fn typed_plain_result(
    &mut self,
    key: (Site, Option<ObjId>),
    module: Option<ModuleIdx>,
    origin: (u32, usize),
    site: Site,
  ) -> ObjId {
    if let Some(p) = self.typed_plain_results.get(&key) {
      return *p;
    }
    let empty = self.var();
    let p = self.alloc(ObjKind::Typed { slot: empty, export: NO_EXPORT }, Some(site));
    self.typed_plain_results.insert(key, p);
    if let Some(m) = module {
      self.typed_result_module.insert(p, m);
    }
    if origin.0 != NO_EXPORT {
      self.typed_result_origin.insert(p, origin);
    }
    p
  }

  /// A call into type-modelled `effect` code (an export, or a value it made). The body is not
  /// analysed (direction 2, Tim's decision of 2026-11-07). Flow follows the declared signature,
  /// with one slot per call site standing for every type parameter, and only what may hold a
  /// service is tracked:
  /// - each argument, filtered by its declared parameter type (concrete types pass no object,
  ///   `unknown`/`any` is ignored and walked for protocols), then by the admission filter
  ///   (`admit`: a value that reaches no callable, Effect value, key, container or opaque value
  ///   is stood for by the site's stand-in), contributes to the slot (`typed_arg`): a plain value
  ///   joins it, a type-modelled value's contents flow in, an Effect contributes its success, an
  ///   array its elements, and callbacks (direct, or in an options object that may hold a
  ///   service) run with the slot and their results join it;
  /// - a key handed over keeps all its methods (the code may provide or read it);
  /// - stateful containers: one program-wide pool per module. A call that may store an argument
  ///   into a value another argument holds (`Dts::container_shape`, or no signature) hands what it is
  ///   handed to its module's pool, and every call into the module reads the pool. This replaces
  ///   writing into the slots of the values handed over, so it holds wherever the container
  ///   lives (nested in a record, or behind a stand-in);
  /// - calling a value it made (a curried result, a method) reads that value's contents, and
  ///   uses its module's pool; calling a stand-in with arguments taints it;
  /// - the result follows the declared return: an `Effect` (succeeding with a summary, and with
  ///   the slot when its success is a type parameter), the slot for a bare type parameter, a
  ///   primitive, or a summary holding the slot. Without a signature it is both.
  fn typed_call(&mut self, callee: ObjId, callee_slot: VarId, export: u32, call: &Call) {
    // Diagnostic only (bound A): every call into type-modelled code is an opaque host call.
    if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_OPAQUE").is_some() {
      self.escape_args(call, Flavour::Host);
      self.seed(call.ret, self.top_host);
      return;
    }
    let stand_in = self.top_effect;
    if self.debug {
      let e = self.typed_call_counts.entry((call.site.module.raw(), call.site.start)).or_default();
      e.0 += 1;
      e.1.insert(callee.raw());
    }
    if call.is_new {
      // A class `effect` exports runs its body on what it is constructed with: class code.
      for a in &call.args {
        let (Arg::Var(v) | Arg::Spread(v)) = a;
        self.constrain(*v, Constraint::ClassEscape(callee, CLASS_TYPED_NEW));
      }
    }
    let module = self.typed_module(callee);
    // Any call into a module's type-modelled code may run its unseen reads: through an export,
    // a method read off one, or a value one of its calls made (the consumer check in `mod.rs`).
    if let Some(m) = module {
      self.typed_called_modules.insert(m);
    }
    // One result per source call site and export: the clones of a function share it, and so do
    // the values a method call is made on (a result per receiver would chain without bound when
    // a value's own result is called again, `loop(x.pipe(…))`).
    let key_site = if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_CLONE_SITES").is_some() {
      call.site
    } else {
      Site { sub: 0, ..call.site }
    };
    let key = (key_site, if export == NO_EXPORT { None } else { Some(callee) });
    let (result, handed) = match self.typed_results.get(&key) {
      Some(r) => *r,
      None => {
        let slot = self.var();
        // What the call is handed, admitted, as a container may store it.
        let handed = self.var();
        let back = self.var();
        self.constrain(handed, Constraint::SlotIn(back, stand_in));
        // What the call hands back may be any service-free value it was handed (`admit_typed`).
        self.seed(slot, self.top_effect);
        let r = self.alloc(ObjKind::Typed { slot, export: NO_EXPORT }, Some(call.site));
        self.typed_results.insert(key, (r, handed));
        self.typed_backs.insert(handed, back);
        (r, handed)
      }
    };
    if let Some(m) = module {
      self.typed_result_module.entry(result).or_insert(m);
    }
    let nargs = call.args.len();
    if export != NO_EXPORT {
      self.typed_result_origin.entry(result).or_insert((export, nargs));
    }
    let ObjKind::Typed { slot, .. } = self.objs[result].kind else { unreachable!() };
    let back = self.typed_backs[&handed];
    if export == NO_EXPORT && self.typed_receivers.insert((result, callee)) {
      // A call on a value it made: what that value holds flows into the result.
      if self.is_stand_in(callee) {
        self.seed(slot, callee);
      } else {
        self.copy(callee_slot, slot);
      }
    }
    if export == NO_EXPORT {
      if let Some(this) = call.this {
        // A method call has no signature: it bails what it reaches (below). Its receivers are
        // the objects whose `name` is the summary, not every value `this` may hold (a value the
        // summary holds, read as its result, calls its own method).
        let c = match self.method_names.get(&call.site) {
          Some(name) => Constraint::TypedReceiver(callee, name.clone(), slot, back, call.site),
          None => Constraint::TypedArg(slot, back, call.site, 0, false, false),
        };
        self.constrain(this, c);
      }
    }
    let sig = if export != NO_EXPORT {
      self.typed_sig(export, nargs)
    } else if call.this.is_none() && !call.is_new {
      self.typed_result_origin.get(&callee).copied().and_then(|o| self.returned_sig(o, nargs))
    } else {
      None
    };
    // A call the summary can't resolve (a method, or a value with no known signature) bails
    // the services it reaches: what it holds and is handed goes to unseen code. A method called
    // with no arguments hands that code nothing new: it may return what the value holds (the
    // result has it) and run the functions the value holds (which run with its slot already,
    // `slot_in`), so what the value holds stays where it is.
    let getter = call.this.is_some() && call.args.is_empty();
    if sig.is_none() && !call.is_new && !getter {
      self.constrain(slot, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
      self.constrain(handed, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
    }
    // The module's container pool, used by calls whose declared shape may store into or read a
    // container. Calling a value an export returned (a data-last form) has that export's shape;
    // a method call, or a call without a signature, may do both.
    if let Some(m) = module {
      let shape = if call.is_new {
        // A class's instance holds what it is constructed with (its slot).
        Some((false, false))
      } else if export != NO_EXPORT {
        sig.as_ref().map(|s| (s.stores, s.reads))
      } else if call.this.is_some() {
        // A method call: it may hand back what the value holds, and stores only if the module
        // declares a method that takes a value of its type's parameter.
        let stores = self.module_methods_store(m);
        Some((stores, true))
      } else {
        self
          .typed_result_origin
          .get(&callee)
          .copied()
          .and_then(|(e, n)| self.typed_sig(e, n))
          .map(|s| (s.stores, s.reads))
      };
      // A call the summary can't resolve (a value with no known signature, called as a function)
      // bails the services it is handed: they escape to code the pass can't see.
      let (stores, reads) = match shape {
        Some(s) => s,
        None => {
          self.constrain(handed, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
          (false, true)
        }
      };
      if stores || reads {
        let pool = self.typed_pool(m);
        if reads {
          self.copy(pool, slot);
        }
        if stores {
          self.copy(back, pool);
          if self.debug {
            let who = if export != NO_EXPORT {
              format!("export {}", self.typed_exports[export as usize].name)
            } else if call.this.is_some() {
              format!("method call at {:?}", (call.site.module.raw(), call.site.start))
            } else {
              match self.typed_result_origin.get(&callee) {
                Some((e, n)) => format!("result of {}/{n}", self.typed_exports[*e as usize].name),
                None => format!("call of value at {:?}", (call.site.module.raw(), call.site.start)),
              }
            };
            self.pool_writers.entry(m).or_default().insert(who);
          }
        }
      }
    }
    let pool = self.ignored_pool();
    for (i, a) in call.args.iter().enumerate() {
      let v = match a {
        Arg::Var(v) => *v,
        Arg::Spread(v) => self.typed_spread_elems(*v),
      };
      let ty = sig.as_ref().and_then(|s| s.params.get(i).copied().flatten().or(s.rest));
      let generic =
        sig.as_ref().is_none_or(|s| s.generic.get(i).copied().unwrap_or(s.rest_generic));
      // The argument as the declared type admits it: the variable itself when the position is
      // open, one filtered copy per (variable, type) otherwise. A copy per application would
      // copy the same values once for every callee the call reaches.
      let arg = match ty {
        None => v,
        Some(t) => self.typed_filtered_arg(v, t),
      };
      if generic {
        // A callback declared to return a value that holds no service goes without its return.
        let prim = sig.as_ref().is_some_and(|s| s.free_returns.get(i).copied().unwrap_or(false));
        // Functions go without their returns here: `typed_callback` (below) tracks those.
        self.constrain(arg, Constraint::AdmitTyped(handed, true));
        // Without a signature, the call hands what it holds to unseen code (below), so its
        // callbacks run with unknown arguments.
        let mask = sig.as_ref().map_or(0, |s| s.masks.get(i).copied().unwrap_or(0xFF));
        let opaque = sig.as_ref().is_some_and(|s| s.opaque.get(i).copied().unwrap_or(false));
        self.constrain(arg, Constraint::TypedArg(slot, back, call.site, mask, prim, opaque));
      } else {
        self.constrain(arg, Constraint::TypedConcrete(ty.unwrap_or(u32::MAX), call.site));
      }
      // Effect's code may call a value's implicit protocol methods (the walk skips values that
      // reach no callable).
      self.copy(arg, pool);
    }
    let ret = sig.map_or(super::dts::RetShape { unknown: true, ..Default::default() }, |s| s.ret);
    let plain = (ret.concrete || ret.effect_concrete)
      .then(|| self.typed_plain_result(key, module, (export, nargs), call.site));
    if ret.concrete {
      if let Some(p) = plain {
        self.seed(call.ret, p);
      }
    }
    if ret.effect {
      let (eff, success, _) = self.new_effect(call.site, 41);
      if ret.effect_other || ret.effect_param {
        self.seed(success, result);
      }
      if ret.effect_concrete {
        if let Some(p) = plain {
          self.seed(success, p);
        }
      }
      if ret.effect_param {
        self.copy(slot, success);
      }
      // A value that comes back only through a service-free container (a Schedule's output,
      // assumption 4; a Sink's result, assumption 6): an unknown value that holds no service. A
      // string among them may still name a member.
      if ret.effect_free {
        self.seed(success, self.top_host);
      }
      self.seed(call.ret, eff);
    }
    if ret.free {
      self.seed(call.ret, self.top_host);
    }
    if ret.param || ret.unknown {
      self.copy(slot, call.ret);
    }
    if ret.prim {
      self.seed(call.ret, self.prim);
    }
    if ret.other || ret.unknown {
      self.seed(call.ret, result);
    }
  }

  // == direction 2: the admission filter

  /// Whether the object may hold a service (direction 2's admission test, over-approximate):
  /// whether anything it reaches through fields, elements, prototypes, spread sources, unknown
  /// stores, a summary's slot or a promise's value is a callable, an Effect value, a key or
  /// container, a summary that may stand for anything, or a value from code the pass can't see.
  /// A service has methods, so a value that reaches none of these holds no service, and nothing
  /// read from it can call or name one. Strings, numbers and other primitives hold nothing. The
  /// answer only turns from false to true as the solve grows (`live` keeps the positives), so a
  /// held-back object is re-checked whenever the solve settles (`recheck_deferred`,
  /// `settle_admissions`).
  fn may_hold(&mut self, obj: ObjId) -> bool {
    self.reaches(obj, false)
  }

  /// Whether the object reaches code: a callable, or anything `may_hold` counts. A walk, an
  /// adoption or effect code calling the handlers it is handed can only run code the value
  /// reaches, so a value that reaches none has nothing for them to do.
  fn reaches_code(&mut self, obj: ObjId) -> bool {
    self.reaches(obj, true)
  }

  /// `may_hold` (`code` false) and `reaches_code` (`code` true). A service is a value provided
  /// under a key (`all_values`), so a value holds one only if it reaches a provided value, a
  /// summary that may stand for anything (a `Wrap`), a value from code the pass can't see, a
  /// key, a container of services or an Effect. A function, class or builtin that holds none
  /// of these is no service: what code may do with one it gets back (call it, read or write
  /// it, hand it on) goes through the stand-in, which releases it (`taint`) or applies the use
  /// to it (`held_var`).
  fn reaches(&mut self, obj: ObjId, code: bool) -> bool {
    const LIMIT: usize = 1 << 17;
    // A key that becomes never-pruned no longer counts (`prunable_service`).
    if !code && self.live_never_prune != self.never_prune.len() {
      self.live_never_prune = self.never_prune.len();
      self.live.clear();
    }
    let live = if code { &self.live_code } else { &self.live };
    if live.contains(&obj) {
      return true;
    }
    // Each object and variable is visited once (`Marks`): the variables of a large record or
    // summary often share their points-to sets, so the walk would otherwise push the same
    // objects many times over. Objects an earlier walk over the same graph found nothing from
    // are skipped too, unless the walks of this graph have visited more than `LIMIT` objects
    // between them: the walk then starts over alone, so it stops at `LIMIT` as it would alone.
    let mode = usize::from(code);
    self.marks[mode].begin(
      self.objs.len(),
      self.vars.len(),
      (self.graph_version, self.never_prune.len()),
    );
    if !self.marks[mode].visit_obj(obj) {
      return false;
    }
    let mut alone = self.marks[mode].visited == 0;
    let mut stack = vec![obj];
    let mut vars: Vec<VarId> = vec![];
    while let Some(o) = stack.pop() {
      self.marks[mode].visited += 1;
      if self.marks[mode].visited > LIMIT && !alone {
        self.marks[mode].restart();
        self.marks[mode].visit_obj(obj);
        alone = true;
        stack.clear();
        stack.push(obj);
        continue;
      }
      let known = if code { &self.live_code } else { &self.live };
      // A provided value (opaque values provided through unknown keys are no program service).
      // Primitives hold nothing, and a primitive service has no method to remove.
      let provided = !code
        && !matches!(
          self.objs[o].kind,
          ObjKind::Top(_)
            | ObjKind::External { .. }
            | ObjKind::Str(_)
            | ObjKind::Num(_)
            | ObjKind::Prim
            | ObjKind::AllKeys
        )
        && self.prunable_service(o);
      if known.contains(&o) || provided || self.marks[mode].visited > LIMIT {
        self.marks[mode].restart();
        if code {
          self.live_code.insert(obj)
        } else {
          self.live.insert(obj)
        };
        return true;
      }
      let ob = &self.objs[o];
      match &ob.kind {
        ObjKind::Str(_) | ObjKind::Num(_) | ObjKind::Prim | ObjKind::AllKeys => continue,
        // What only reads, provides or runs through a stand-in stays sound behind one: a
        // Context or Layer (lookups and provision through a stand-in see every key), a value
        // from unseen code or a builtin (a call through a stand-in releases it), an Effect's
        // body (run when the Effect is built; only its success matters).
        ObjKind::Context { .. }
        | ObjKind::Layer { .. }
        | ObjKind::Runtime { .. }
        | ObjKind::Fiber
        | ObjKind::MemoMap
        | ObjKind::Top(_)
        | ObjKind::External { .. }
        | ObjKind::Namespace
        | ObjKind::Builtin { .. }
          if !code =>
        {
          continue;
        }
        ObjKind::Effect { success, .. } if !code => vars.push(*success),
        // Every access path on a summary yields the values it wraps.
        ObjKind::Wrap(inner) if !code => vars.push(*inner),
        // A host prototype (`Array.prototype`) holds no program service: writing to one bails
        // everything already.
        ObjKind::Object | ObjKind::Array if !code && ob.builtin_proto => continue,
        ObjKind::Object | ObjKind::Array | ObjKind::Closure(_)
          if !code || matches!(ob.kind, ObjKind::Object | ObjKind::Array) =>
        {
          if code && (ob.builtin_proto || self.accessors.contains_key(&o)) {
            self.marks[mode].restart();
            self.live_code.insert(obj);
            return true;
          }
          vars.extend(ob.fields.values().copied());
          vars.extend(ob.elem);
          vars.extend(ob.proto);
          vars.extend(ob.spread_from.iter().copied());
          vars.extend(ob.unknown_stores.iter().copied());
        }
        // A summary may be an effect function (`HttpBody.file` as a `toJSON`): code.
        ObjKind::Typed { slot, .. } if !code => vars.push(*slot),
        ObjKind::Promise { value } => vars.push(*value),
        _ => {
          self.marks[mode].restart();
          if code {
            self.live_code.insert(obj)
          } else {
            self.live.insert(obj)
          };
          return true;
        }
      }
      for v in vars.drain(..) {
        if self.marks[mode].visit_var(v) {
          let marks = &mut self.marks[mode];
          stack.extend(self.vars[v].pts.iter().copied().filter(|p| marks.visit_obj(*p)));
        }
      }
    }
    false
  }

  /// Debug: the path `may_hold` follows from `obj` to what it counts as live.
  fn service_witness(&self, obj: ObjId) -> String {
    let mut seen: FxHashSet<ObjId> = FxHashSet::default();
    let mut stack: Vec<(ObjId, Vec<String>)> = vec![(obj, vec![])];
    let desc = |a: &Self, o: ObjId| {
      format!(
        "{} {:?}",
        format!("{:?}", a.objs[o].kind).split([' ', '(', '{']).next().unwrap_or(""),
        a.objs[o].site.map(|s| (s.module.raw(), s.start))
      )
    };
    while let Some((o, path)) = stack.pop() {
      if !seen.insert(o) || seen.len() > 20000 {
        continue;
      }
      let ob = &self.objs[o];
      let mut path = path;
      if path.len() > 12 {
        continue;
      }
      path.push(desc(self, o));
      if !matches!(
        ob.kind,
        ObjKind::Top(_)
          | ObjKind::External { .. }
          | ObjKind::Str(_)
          | ObjKind::Num(_)
          | ObjKind::Prim
          | ObjKind::AllKeys
      ) && self.vars[self.all_values].pts.contains(&o)
      {
        return format!("{path:?} (provided)");
      }
      let mut vars: Vec<(String, VarId)> = vec![];
      match &ob.kind {
        ObjKind::Str(_)
        | ObjKind::Num(_)
        | ObjKind::Prim
        | ObjKind::AllKeys
        | ObjKind::Context { .. }
        | ObjKind::Layer { .. }
        | ObjKind::Runtime { .. }
        | ObjKind::Fiber
        | ObjKind::MemoMap
        | ObjKind::Top(_)
        | ObjKind::External { .. }
        | ObjKind::Namespace
        | ObjKind::Builtin { .. } => {}
        ObjKind::Effect { success, .. } => vars.push(("success".into(), *success)),
        ObjKind::Wrap(inner) => vars.push(("wrapped".into(), *inner)),
        ObjKind::Object | ObjKind::Array if ob.builtin_proto => {}
        ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) => {
          vars.extend(ob.fields.iter().map(|(k, v)| (k.to_string(), *v)));
          vars.extend(ob.elem.map(|v| ("[]".to_string(), v)));
          vars.extend(ob.proto.map(|v| ("__proto__".to_string(), v)));
          vars.extend(ob.spread_from.iter().map(|v| ("...".to_string(), *v)));
          vars.extend(ob.unknown_stores.iter().map(|v| ("[?]=".to_string(), *v)));
        }
        ObjKind::Typed { slot, .. } => vars.push(("slot".into(), *slot)),
        ObjKind::Promise { value } => vars.push(("value".into(), *value)),
        _ => return format!("{path:?} (live kind)"),
      }
      for (k, v) in vars {
        for x in &self.vars[v].pts {
          let mut p = path.clone();
          p.push(k.clone());
          stack.push((*x, p));
        }
      }
    }
    "none".to_string()
  }

  /// Whether the object is provided under a key that may be pruned: a Reference, or a key the
  /// runtime reads itself, keeps every method anyway, so tracking its values gains nothing.
  fn prunable_service(&self, o: ObjId) -> bool {
    if !self.vars[self.all_values].pts.contains(&o) {
      return false;
    }
    self.vars[self.unknown_key_values].pts.contains(&o)
      || self
        .key_values
        .iter()
        .any(|(k, v)| !self.never_prune.contains(k) && self.vars[*v].pts.contains(&o))
  }

  fn is_stand_in(&self, obj: ObjId) -> bool {
    self.stand_ins.contains_key(&obj)
  }

  /// A call through a stand-in: the call reaches the callables it holds (`held_var`). A member
  /// read off a stand-in is the stand-in, so the call may also be a method of a record or array
  /// it holds: one that reaches code taints it. Data with no code in it has no method to call.
  /// A call through a stand-in: the functions it holds are called; the call is otherwise a
  /// call into effect code on a value that holds no service (a held summary's method: its
  /// callbacks run, and it returns what it was handed that may hold a service, or service-free
  /// data, the stand-in itself). `stand_in_called` releases it where that is not enough.
  fn stand_in_call(&mut self, stand_in: ObjId, held_var: VarId, call: &Call) {
    self.constrain(held_var, Constraint::CallFn(Box::new(call.clone())));
    let returns = self.stand_ins[&stand_in].returns;
    for a in &call.args {
      let v = match a {
        Arg::Var(v) | Arg::Spread(v) => *v,
      };
      self.constrain(v, Constraint::TypedArg(returns, returns, call.site, 0xFF, false, false));
    }
    self.seed(call.ret, stand_in);
    self.copy(returns, call.ret);
    // A plain call calls the held value itself, and a record or array is no function. A method
    // call (`x.m()`, whose member read off a stand-in is the stand-in) may call a held record's
    // method.
    if call.this.is_some() {
      self.stand_in_called(stand_in);
    }
  }

  fn stand_in_called(&mut self, stand_in: ObjId) {
    let Some(s) = self.stand_ins.get_mut(&stand_in) else { return };
    if s.tainted {
      return;
    }
    s.called = true;
    let held: Vec<ObjId> = s.held.iter().map(|(o, _)| *o).collect();
    if let Some(o) = held.into_iter().find(|o| self.held_call_needs_release(*o)) {
      let why = if self.debug {
        format!(
          "call; holds {} from {:?}",
          format!("{:?}", self.objs[o].kind).split([' ', '(', '{']).next().unwrap_or(""),
          self.objs[o].site.map(|s| (s.module.raw(), s.start))
        )
      } else {
        String::new()
      };
      self.taint_by(stand_in, &why);
    }
  }

  /// Whether a call through a stand-in holding `obj` must release it: `obj` is a record or array
  /// with code (one of its methods may be what is called), or a summary from a module whose
  /// methods may store their arguments (`Dts::methods_store`).
  fn held_call_needs_release(&mut self, obj: ObjId) -> bool {
    match self.objs[obj].kind {
      ObjKind::Object | ObjKind::Array => self.reaches_code(obj),
      ObjKind::Typed { .. } if !self.is_stand_in(obj) => match self.typed_module(obj) {
        Some(m) => self.module_methods_store(m),
        None => false,
      },
      _ => false,
    }
  }

  fn module_methods_store(&mut self, m: ModuleIdx) -> bool {
    if let Some(b) = self.methods_store.get(&m) {
      return *b;
    }
    let path = self.typed_exports.iter().find(|e| e.module == m).map(|e| e.dts.clone());
    let b = path.is_none_or(|p| self.dts.methods_store(&p));
    self.methods_store.insert(m, b);
    b
  }

  /// Direction 2's admission filter at a call into type-modelled code: a value that may hold a
  /// service flows into `dst` (a slot, or what the call hands its module's pool) as itself.
  /// Anything else goes to code the pass can't see: effect code holds it, may call the functions
  /// it reaches with unknown arguments (and what they return goes there too), and a service
  /// written into it later bails its key. Every value a call with a generic result hands back
  /// includes `top_ext` for these (`typed_call`), so whatever the caller does with one is what
  /// it would do with a value from unseen code. Primitives hold nothing and are covered by it.
  fn admit_typed(&mut self, obj: ObjId, dst: VarId) {
    self.admit_typed_with(obj, dst, true);
  }

  /// `admit_typed`; a function that goes to effect's code goes without its return when `ret`
  /// is false (`escape_with`).
  fn admit_typed_with(&mut self, obj: ObjId, dst: VarId, ret: bool) {
    let filter = self.filter_vars.contains(&dst);
    if matches!(
      self.objs[obj].kind,
      ObjKind::Str(_) | ObjKind::Num(_) | ObjKind::Prim | ObjKind::AllKeys
    ) {
      // A host builtin's filter (`admitted_values`) hands primitives out as they are.
      if filter {
        self.seed(dst, obj);
      }
      return;
    }
    // A boundary's opaque value is one object standing for code that is not analysed (a class
    // body, a module hole): calling it with a service promotes or bails that code, which is
    // what holding it back to effect's code would lose.
    if self.may_hold(obj)
      || self.opaque_class.contains_key(&obj)
      || (self.callable_filter_vars.contains(&dst)
        && matches!(self.objs[obj].kind, ObjKind::Closure(_)))
    {
      self.admission_stats.0 += 1;
      self.seed(dst, obj);
      return;
    }
    // An Effect, promise or type-modelled result is decided when the solve settles: what it
    // succeeds with or holds often arrives after the handover (a fiber handed to `addObserver`
    // before its Effect has run, a `Stream.make(record)` handed to `Stream.run` before the
    // record reaches its slot), and the decision must not depend on which the solve met first.
    // A result that comes to hold a service flows as itself; one that holds none goes to
    // effect's code then, without dragging what it will never hold along. Records, arrays and
    // functions are decided now: deciding them at the fixpoint too admits values whose flow
    // the budget can't carry (`distilled_stripe` grows from 0.8M to 7.6M entries).
    if !matches!(
      self.objs[obj].kind,
      ObjKind::Effect { .. } | ObjKind::Promise { .. } | ObjKind::Typed { .. }
    ) {
      self.admission_stats.1 += 1;
      self.hold_back(obj, ret);
      if filter {
        self.seed(dst, self.top_effect);
      }
      return;
    }
    if self.held_admissions_seen.insert((obj, dst)) {
      self.held_admissions.push((obj, dst));
    }
  }

  /// At a point where the solve has settled: admits every held-back Effect and promise that
  /// may now hold a service, and keeps the rest waiting. Returns whether any was admitted; the
  /// solve then runs again, so what an admission hands on (the success of another waiting
  /// value, directly or through copies) is seen before anything waiting is given up
  /// (`release_admissions`). Admissions only add, so the set admitted at the end does not
  /// depend on the order the solve met the handovers in.
  fn settle_admissions(&mut self) -> bool {
    if self.held_admissions.is_empty() {
      return false;
    }
    let pending = std::mem::take(&mut self.held_admissions);
    let mut keep = Vec::with_capacity(pending.len());
    let mut admitted = false;
    // One walk per value: a value handed to many sites is held once per site. What an
    // admission seeds is seen on the next pass, after the solve has run again.
    let mut holds: FxHashMap<ObjId, bool> = FxHashMap::default();
    for (obj, dst) in pending {
      let hold = match holds.get(&obj) {
        Some(h) => *h,
        None => {
          let h = self.may_hold(obj);
          holds.insert(obj, h);
          h
        }
      };
      if hold {
        self.admission_stats.0 += 1;
        self.seed(dst, obj);
        admitted = true;
      } else {
        keep.push((obj, dst));
      }
    }
    keep.append(&mut self.held_admissions);
    self.held_admissions = keep;
    admitted
  }

  /// Once the solve has settled and no waiting value was admitted (`settle_admissions`): every
  /// Effect and promise still waiting holds no service at this fixpoint and goes to effect's
  /// code, all at once (`hold_back` only adds constraints, so no release changes what another
  /// is decided on). What it succeeds with later still goes there as it arrives (`EscapeOut`),
  /// so a service reaching it after this bails (sound, less precise). Returns whether any went.
  fn release_admissions(&mut self) -> bool {
    if self.held_admissions.is_empty() {
      return false;
    }
    for (obj, dst) in std::mem::take(&mut self.held_admissions) {
      self.admission_stats.1 += 1;
      self.hold_back(obj, true);
      if self.filter_vars.contains(&dst) {
        self.seed(dst, self.top_effect);
      }
    }
    true
  }

  /// A value handed to type-modelled code that holds no service: it goes to effect's code. A
  /// function goes without its return when `ret` is false.
  fn hold_back(&mut self, obj: ObjId, ret: bool) {
    match self.objs[obj].kind {
      // A summary, Effect or promise that holds no prunable service yet may come to hold one
      // (its slot, success or value grows): what it holds goes to unseen code as it arrives, so
      // a service reaching it later bails.
      ObjKind::Object
      | ObjKind::Array
      | ObjKind::Closure(_)
      | ObjKind::Builtin { .. }
      | ObjKind::Typed { .. }
      | ObjKind::Wrap(_) => {
        self.escape_with(obj, Flavour::Ext, Some(EFFECT_ESCAPE), ret);
      }
      ObjKind::Effect { success, .. } => self.constrain(success, Constraint::EscapeOut),
      ObjKind::Promise { value } => self.constrain(value, Constraint::EscapeOut),
      _ => {}
    }
  }

  /// Something may write into, hand over or call what the stand-in stands for: from now on its
  /// values flow as themselves.
  fn taint_by(&mut self, stand_in: ObjId, why: &str) {
    let Some(s) = self.stand_ins.get_mut(&stand_in) else { return };
    if s.tainted {
      return;
    }
    s.tainted = true;
    let held = std::mem::take(&mut s.held);
    if self.debug {
      let site = self
        .site_stand_ins
        .iter()
        .find(|(_, o)| **o == stand_in)
        .map(|(s, _)| (s.module.raw(), s.start));
      *self
        .taint_log
        .entry(format!("{why} stand-in of {site:?} holding {}", held.len()))
        .or_default() += 1;
    }
    self.admission_stats.2 += 1;
    for (obj, dst) in held {
      self.seed(dst, obj);
    }
  }

  /// `src` filtered by the admission test, as the variable an unknown-key store copies into the
  /// target's fields (one per source variable). A value stored under a dynamic key reaches
  /// every field, so only what may hold a prunable service flows as itself; anything else goes
  /// to unseen code (`admit_typed`), and the fields hold `top_ext` for it: reading one back is
  /// reading a value from unseen code, and a service written into it later bails.
  /// Host builtins that copy structure hand out values through the same filter.
  pub fn admitted_values(&mut self, src: VarId) -> VarId {
    if let Some(v) = self.unknown_store_filters.get(&src) {
      return *v;
    }
    let v = self.var();
    self.unknown_store_filters.insert(src, v);
    self.filter_vars.insert(v);
    self.constrain(src, Constraint::AdmitTyped(v, false));
    v
  }

  /// `admitted_values`, letting the program's functions through as themselves.
  pub fn admitted_callables(&mut self, src: VarId) -> VarId {
    if let Some(v) = self.callable_filters.get(&src) {
      return *v;
    }
    let v = self.var();
    self.callable_filters.insert(src, v);
    self.filter_vars.insert(v);
    self.callable_filter_vars.insert(v);
    self.constrain(src, Constraint::AdmitTyped(v, false));
    v
  }

  /// Runs the held-back work whose object may now hold a service; returns whether any ran.
  fn recheck_deferred(&mut self) -> bool {
    let called: Vec<ObjId> =
      self.stand_ins.iter().filter(|(_, s)| s.called && !s.tainted).map(|(o, _)| *o).collect();
    let mut tainted_any = false;
    for o in called {
      self.stand_in_called(o);
      tainted_any |= self.stand_ins[&o].tainted;
    }
    if self.deferred.is_empty() {
      return tainted_any;
    }
    let pending = std::mem::take(&mut self.deferred);
    let mut ran = false;
    let mut keep = vec![];
    for d in pending {
      let obj = match &d {
        Deferred::Walk(o, ..) | Deferred::Adopt(o, _) | Deferred::Concrete(o, ..) => *o,
      };
      if !self.reaches_code(obj) {
        keep.push(d);
        continue;
      }
      ran = true;
      match d {
        Deferred::Walk(o, walk, pool, arg, site) => self.walk(o, walk, pool, arg, site),
        Deferred::Adopt(o, dst) => self.adopt(o, dst),
        Deferred::Concrete(o, ty, site) => self.typed_concrete(o, ty, site),
      }
    }
    keep.append(&mut self.deferred);
    self.deferred = keep;
    ran || tainted_any
  }

  /// Diagnostic: entries by what created the variables holding them, overall and for the
  /// constraint-free one-copy relays and no-copy sinks.
  pub fn origin_census(&self) -> String {
    let mut all: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
    let mut relays: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
    let mut sinks: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
    for (v, var) in self.vars.iter_enumerated() {
      let tag = self.var_origin.get(v.index()).copied().unwrap_or("?");
      let n = var.pts.len();
      let e = all.entry(tag).or_default();
      e.0 += n;
      e.1 += 1;
      if var.constraints.is_empty() {
        let m = match var.copies.len() {
          0 => &mut sinks,
          1 => &mut relays,
          _ => continue,
        };
        let e = m.entry(tag).or_default();
        e.0 += n;
        e.1 += 1;
      }
    }
    let top = |m: FxHashMap<&str, (usize, usize)>| -> Vec<String> {
      let mut v: Vec<_> = m.into_iter().collect();
      v.sort_by_key(|x| std::cmp::Reverse(x.1.0));
      v.iter().take(25).map(|(k, (n, c))| format!("{n} in {c} vars: {k}")).collect()
    };
    format!(
      "by origin {:#?}; relays by origin {:#?}; sinks by origin {:#?}",
      top(all),
      top(relays),
      top(sinks)
    )
  }

  /// Debug census of the type model: pool sizes by module, the largest slots and what fills
  /// them, and where the points-to entries are.
  #[expect(clippy::too_many_lines, reason = "one debug report")]
  pub fn typed_census(&self, names: &dyn Fn(ModuleIdx) -> String) -> String {
    let mut pools: Vec<(usize, String)> =
      self.typed_pools.iter().map(|(m, v)| (self.vars[*v].pts.len(), names(*m))).collect();
    pools.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut slots: Vec<(usize, ObjId)> = self
      .objs
      .iter_enumerated()
      .filter_map(|(o, ob)| match ob.kind {
        ObjKind::Typed { slot, .. } => Some((self.vars[slot].pts.len(), o)),
        _ => None,
      })
      .collect();
    slots.sort_by_key(|x| std::cmp::Reverse(x.0));
    let total: usize = slots.iter().map(|x| x.0).sum();
    let describe = |o: ObjId| -> String {
      let ob = &self.objs[o];
      let what = match ob.kind {
        ObjKind::Typed { export, .. } if export != NO_EXPORT => {
          format!("export {}", self.typed_exports[export as usize].name)
        }
        _ if self.is_stand_in(o) => "stand-in".to_string(),
        _ => format!(
          "result {:?} of {}",
          ob.site.map(|s| (s.module.raw(), s.start)),
          self.typed_result_module.get(&o).map(|m| names(*m)).unwrap_or_default()
        ),
      };
      let ObjKind::Typed { slot, .. } = ob.kind else { return what };
      let mut kinds: FxHashMap<String, usize> = FxHashMap::default();
      for x in &self.vars[slot].pts {
        let k = if self.is_stand_in(*x) {
          "StandIn".to_string()
        } else {
          format!("{:?}", self.objs[*x].kind)
            .split([' ', '(', '{'])
            .next()
            .unwrap_or("")
            .to_string()
        };
        *kinds.entry(k).or_default() += 1;
      }
      format!("{what} {kinds:?}")
    };
    let top: Vec<String> =
      slots.iter().take(12).map(|(n, o)| format!("{n}: {}", describe(*o))).collect();
    let mut origins: FxHashMap<String, usize> = FxHashMap::default();
    if let Some((_, o)) = slots.first() {
      if let ObjKind::Typed { slot, .. } = self.objs[*o].kind {
        for x in &self.vars[slot].pts {
          let ob = &self.objs[*x];
          let k = format!(
            "{} {:?}",
            format!("{:?}", ob.kind).split([' ', '(', '{']).next().unwrap_or(""),
            ob.site.map(|s| (names(s.module), s.start))
          );
          *origins.entry(k).or_default() += 1;
        }
      }
    }
    let mut origins: Vec<(String, usize)> = origins.into_iter().collect();
    origins.sort_by_key(|x| std::cmp::Reverse(x.1));
    // Why the first array or record of the largest slot passed the service test.
    let witness = slots.first().and_then(|(_, o)| match self.objs[*o].kind {
      ObjKind::Typed { slot, .. } => self.vars[slot]
        .pts
        .iter()
        .copied()
        .find(|x| matches!(self.objs[*x].kind, ObjKind::Array | ObjKind::Object)),
      _ => None,
    });
    let witness = witness.map(|w| self.service_witness(w));
    let top = {
      let mut t = top;
      t.push(format!("first slot by origin: {:?}", &origins[..origins.len().min(40)]));
      t.push(format!("witness: {witness:?}"));
      t
    };
    let typed_vars: usize = self.vars.iter().filter(|v| v.pts.len() > 100).count();
    // Points-to entries by the origin of the object held: what fills the budget.
    let mut by_origin: FxHashMap<(String, Option<(u32, u32)>), usize> = FxHashMap::default();
    for v in &self.vars {
      for o in &v.pts {
        let ob = &self.objs[*o];
        let kind = if self.is_stand_in(*o) {
          "StandIn".to_string()
        } else {
          format!("{:?}", ob.kind).split([' ', '(', '{']).next().unwrap_or("").to_string()
        };
        *by_origin.entry((kind, ob.site.map(|s| (s.module.raw(), s.start)))).or_default() += 1;
      }
    }
    let mut by_origin: Vec<_> = by_origin.into_iter().collect();
    by_origin.sort_by_key(|x| std::cmp::Reverse(x.1));
    let by_origin: Vec<String> =
      by_origin.iter().take(30).map(|((k, s), n)| format!("{n} {k} {s:?}")).collect();
    // Points-to entries by the function owning the variable (template and clones together).
    let mut owner: FxHashMap<VarId, (u32, u32)> = FxHashMap::default();
    for f in &self.fns {
      let key = (f.module.raw(), f.span_start);
      for v in f.locals.iter().chain(f.params.iter()).chain([&f.ret, &f.this]) {
        owner.entry(*v).or_insert(key);
      }
    }
    let mut by_fn: FxHashMap<(u32, u32), (usize, usize)> = FxHashMap::default();
    let mut unowned = 0usize;
    for (v, var) in self.vars.iter_enumerated() {
      match owner.get(&v) {
        Some(k) => {
          let e = by_fn.entry(*k).or_default();
          e.0 += var.pts.len();
          e.1 += 1;
        }
        None => unowned += var.pts.len(),
      }
    }
    // The unowned ones: fields, elements and prototypes of objects, by the object's origin.
    let mut field_owner: FxHashMap<VarId, String> = FxHashMap::default();
    for (o, ob) in self.objs.iter_enumerated() {
      let kind = if self.is_stand_in(o) {
        "StandIn".to_string()
      } else {
        format!("{:?}", ob.kind).split([' ', '(', '{']).next().unwrap_or("").to_string()
      };
      let d = format!("{kind} {:?}", ob.site.map(|s| (s.module.raw(), s.start)));
      for (name, v) in &ob.fields {
        field_owner.entry(*v).or_insert(format!("{d} .{name}"));
      }
      if let Some(v) = ob.elem {
        field_owner.entry(v).or_insert(format!("{d} []"));
      }
      if let ObjKind::Typed { slot, .. } = ob.kind {
        field_owner.entry(slot).or_insert(format!("{d} slot"));
      }
    }
    let mut by_field: FxHashMap<String, (usize, usize)> = FxHashMap::default();
    let mut rest = 0usize;
    for (v, var) in self.vars.iter_enumerated() {
      if owner.contains_key(&v) {
        continue;
      }
      match field_owner.get(&v) {
        Some(k) => {
          let e = by_field.entry(k.clone()).or_default();
          e.0 += var.pts.len();
          e.1 += 1;
        }
        None => rest += var.pts.len(),
      }
    }
    // The fattest other model variables, by what they are attached to.
    let mut sig_of: FxHashMap<String, (usize, usize)> = FxHashMap::default();
    for (v, var) in self.vars.iter_enumerated() {
      if owner.contains_key(&v) || field_owner.contains_key(&v) || var.pts.len() < 50 {
        continue;
      }
      let cs: Vec<String> = var
        .constraints
        .iter()
        .take(3)
        .map(|c| format!("{c:?}").split([' ', '(', '{']).next().unwrap_or("").to_string())
        .collect();
      let k = format!("constraints {cs:?}, {} copies", var.copies.len().min(5));
      let e = sig_of.entry(k).or_default();
      e.0 += var.pts.len();
      e.1 += 1;
    }
    let mut sig_of: Vec<_> = sig_of.into_iter().collect();
    sig_of.sort_by_key(|x| std::cmp::Reverse(x.1.0));
    let sig_of: Vec<String> =
      sig_of.iter().take(15).map(|(k, (n, vs))| format!("{n} in {vs} vars: {k}")).collect();
    // The constraint-free variables with one copy: where their values come from and go.
    let describe_var = |v: VarId| -> String {
      if let Some((m, st)) = owner.get(&v) {
        return format!("fn ({m}, {st})");
      }
      if let Some(f) = field_owner.get(&v) {
        return format!("field {f}");
      }
      let cs: Vec<String> = self.vars[v]
        .constraints
        .iter()
        .take(2)
        .map(|c| format!("{c:?}").split([' ', '(', '{']).next().unwrap_or("").to_string())
        .collect();
      format!("model var {cs:?}")
    };
    let mut incoming: FxHashMap<VarId, Vec<VarId>> = FxHashMap::default();
    for (src, var) in self.vars.iter_enumerated() {
      for dst in &var.copies {
        if self.vars[*dst].constraints.is_empty() && self.vars[*dst].copies.len() == 1 {
          incoming.entry(*dst).or_default().push(src);
        }
      }
    }
    let mut relays: FxHashMap<String, (usize, usize)> = FxHashMap::default();
    for (v, var) in self.vars.iter_enumerated() {
      if !var.constraints.is_empty() || var.copies.len() != 1 || var.pts.len() < 50 {
        continue;
      }
      if owner.contains_key(&v) || field_owner.contains_key(&v) {
        continue;
      }
      let dst = *var.copies.iter().next().unwrap();
      let mut froms: Vec<String> = incoming
        .get(&v)
        .map(|srcs| srcs.iter().take(3).map(|s| describe_var(*s)).collect())
        .unwrap_or_default();
      froms.sort();
      froms.dedup();
      let k = format!("from {froms:?} to {}", describe_var(dst));
      let e = relays.entry(k).or_default();
      e.0 += var.pts.len();
      e.1 += 1;
    }
    let mut relays: Vec<_> = relays.into_iter().collect();
    relays.sort_by_key(|x| std::cmp::Reverse(x.1.0));
    let relays: Vec<String> =
      relays.iter().take(15).map(|(k, (n, vs))| format!("{n} in {vs} vars {k}")).collect();
    let mut by_field: Vec<_> = by_field.into_iter().collect();
    by_field.sort_by_key(|x| std::cmp::Reverse(x.1.0));
    let by_field: Vec<String> = std::iter::once(format!("{rest} in other model variables"))
      .chain(by_field.iter().take(25).map(|(k, (n, vs))| format!("{n} in {vs} x {k}")))
      .collect();
    let mut by_fn: Vec<_> = by_fn.into_iter().collect();
    by_fn.sort_by_key(|x| std::cmp::Reverse(x.1.0));
    let by_fn: Vec<String> = std::iter::once(format!("{unowned} in variables no function owns"))
      .chain(by_fn.iter().take(25).map(|(k, (n, vs))| format!("{n} in {vs} vars of fn {k:?}")))
      .collect();
    let mut cb: FxHashMap<(u32, u32, u8), (usize, usize)> = FxHashMap::default();
    for (m, st, mask, slot, _) in &self.callback_log {
      let e = cb.entry((*m, *st, *mask)).or_default();
      e.0 += 1;
      e.1 = e.1.max(if *mask != 0 { self.vars[*slot].pts.len() } else { 0 });
    }
    let mut cbs: Vec<CallbackRow> = cb.into_iter().collect();
    cbs.sort_by_key(|x| std::cmp::Reverse(x.1.0 * x.1.1.max(1)));
    let cbs: Vec<String> =
      cbs.iter().take(12).map(|(k, v)| format!("{k:?}: {} runs, slot {}", v.0, v.1)).collect();
    let mut hot: Vec<((u32, u32), usize, usize)> =
      self.typed_call_counts.iter().map(|(k, (n, c))| (*k, *n, c.len())).collect();
    hot.sort_by_key(|x| std::cmp::Reverse(x.1));
    let total_calls: usize = hot.iter().map(|x| x.1).sum();
    let hot: Vec<String> = std::iter::once(format!("total {total_calls} at {} sites", hot.len()))
      .chain(hot.iter().take(15).map(|(k, n, c)| format!("{k:?}: {n} applications, {c} callees")))
      .collect();
    let fat = self.vars.iter_enumerated().max_by_key(|(_, v)| v.pts.len()).map(|(v, _)| v);
    let fat_desc = fat.map(|v| {
      let mut origins: FxHashMap<String, usize> = FxHashMap::default();
      for x in &self.vars[v].pts {
        let ob = &self.objs[*x];
        let k = format!(
          "{} {:?}",
          format!("{:?}", ob.kind).split([' ', '(', '{']).next().unwrap_or(""),
          ob.site.map(|s| (s.module.raw(), s.start))
        );
        *origins.entry(k).or_default() += 1;
      }
      let mut o: Vec<(String, usize)> = origins.into_iter().collect();
      o.sort_by_key(|x| std::cmp::Reverse(x.1));
      let cs: Vec<String> = self.vars[v]
        .constraints
        .iter()
        .take(6)
        .map(|c| format!("{c:?}").chars().take(160).collect())
        .collect();
      let fns: Vec<String> = self
        .fns
        .iter_enumerated()
        .filter(|(_, f)| f.params.contains(&v) || f.ret == v || f.this == v)
        .take(3)
        .map(|(id, f)| format!("fn {id:?} module {} span {}", f.module.raw(), f.span_start))
        .collect();
      format!(
        "{v:?} holds {:?}; constraints {cs:?}; copies {}; fns {fns:?}",
        &o[..o.len().min(15)],
        self.vars[v].copies.len()
      )
    });
    let writers: Vec<String> = self
      .pool_writers
      .iter()
      .map(|(m, w)| {
        let mut w: Vec<&String> = w.iter().collect();
        w.sort();
        format!("{}: {:?}", names(*m), &w[..w.len().min(12)])
      })
      .collect();
    let mut taints: Vec<(&String, &usize)> = self.taint_log.iter().collect();
    taints.sort_by_key(|x| std::cmp::Reverse(*x.1));
    let taints: Vec<String> = taints.iter().take(25).map(|(k, n)| format!("{n} x {k}")).collect();
    format!(
      "relays {relays:#?}; other variables {sig_of:#?}; entries by field {by_field:#?}; entries by function {by_fn:#?}; entries by origin {by_origin:#?}; callbacks {cbs:?}; hot typed calls {hot:?}; fattest {fat_desc:?}; taints {taints:#?}; pools {:?}; writers {writers:#?}; {} Typed, slot entries {total}; vars over 100 entries {typed_vars}; largest slots {top:#?}",
      &pools[..pools.len().min(10)],
      slots.len()
    )
  }

  /// The container pool of a type-modelled module.
  fn typed_pool(&mut self, module: ModuleIdx) -> VarId {
    if let Some(v) = self.typed_pools.get(&module) {
      return *v;
    }
    let v = self.var();
    self.typed_pools.insert(module, v);
    v
  }

  /// A value entering a slot (or what a call is handed, for its module's pool): an Effect
  /// contributes its success and a promise its value; a function runs with what the slot holds
  /// (a stored callback); a stand-in enters as itself; anything else, a
  /// summary included, goes through the admission filter.
  fn slot_in(&mut self, obj: ObjId, dst: VarId, stand_in: ObjId) {
    match self.objs[obj].kind {
      ObjKind::Effect { success, .. } => {
        self.constrain(success, Constraint::SlotIn(dst, stand_in));
      }
      ObjKind::Promise { value } => self.constrain(value, Constraint::SlotIn(dst, stand_in)),
      // A function a type-modelled value holds may be called by its code (a stored callback): it
      // runs with what the value holds.
      ObjKind::Closure(_) | ObjKind::Builtin { .. } => {
        self.admit_typed(obj, dst);
        if self.typed_slot_fns.insert((obj, dst)) {
          let back = self.var();
          self.typed_callback(obj, dst, back, protocol_site(6), 0xFF, false);
        }
      }
      _ => {
        let _ = stand_in;
        self.admit_typed(obj, dst);
      }
    }
  }

  /// What one argument at a generic position of a type-modelled call contributes to its slot
  /// (`typed_call`). A summary's contents flow on; an Effect contributes its success, a promise
  /// its value; a key or class handed over keeps every method; functions run with the slot as
  /// callbacks, and a record's or array's handlers do too (effect code reads them by name),
  /// unless the position is `opaque`: a value of a type parameter whose constraint names no
  /// member has none read. The value itself enters the slot only if it may hold a service
  /// (`admit_typed`).
  #[expect(clippy::too_many_arguments, reason = "the fields of `Constraint::TypedArg`")]
  fn typed_arg(
    &mut self,
    obj: ObjId,
    slot: VarId,
    back: VarId,
    site: Site,
    mask: u8,
    prim: bool,
    opaque: bool,
  ) {
    // Once per (object, slot, flags): a callback's result feeds the same slot back. A callback
    // handed where its return is open is not skipped for having been handed where it is not.
    if !self.typed_args_seen.insert((obj, slot, prim, opaque)) {
      return;
    }
    let top = self.top_effect;
    match self.objs[obj].kind.clone() {
      // What a call stores into a container goes to its module's pool (`typed_call`).
      ObjKind::Typed { .. } if self.is_stand_in(obj) => self.seed(slot, obj),
      ObjKind::Typed { slot: other, .. } => {
        self.copy(other, slot);
        self.pool_route(obj, back);
      }
      ObjKind::Effect { success, .. } => self.constrain(success, Constraint::SlotIn(slot, top)),
      ObjKind::Promise { value } => self.constrain(value, Constraint::SlotIn(slot, top)),
      ObjKind::Wrap(inner) => {
        self.seed(slot, obj);
        self.constrain(inner, Constraint::SlotIn(slot, top));
      }
      ObjKind::Closure(_) | ObjKind::Builtin { .. } => {
        self.admit_typed_with(obj, slot, false);
        self.typed_callback(obj, slot, back, site, mask, prim);
      }
      ObjKind::Key { strings, .. } => {
        self.seed(slot, obj);
        self.constrain(strings, Constraint::BailKeys);
      }
      ObjKind::Class { .. } => {
        self.seed(slot, obj);
        let strings = self.var();
        self.key_use(obj, &KeyUse::Strings(strings));
        self.constrain(strings, Constraint::BailKeys);
      }
      ObjKind::Array | ObjKind::Object => {
        if self.may_hold(obj) {
          self.admission_stats.0 += 1;
          self.seed(slot, obj);
          if matches!(self.objs[obj].kind, ObjKind::Array) {
            let elem = self.elem(obj);
            self.constrain(elem, Constraint::AdmitTyped(slot, true));
          }
        } else {
          // Its functions run as callbacks (`typed_nested_callbacks`), which track their returns.
          self.admit_typed_with(obj, slot, false);
        }
        if !opaque {
          self.typed_nested_callbacks(obj, slot, back, site, mask);
        }
      }
      _ => self.admit_typed(obj, slot),
    }
  }

  /// An argument at a concrete position of a type-modelled call (direction 2: a value at a
  /// concretely typed position can't flow into a type parameter's slot). What the code may still
  /// do with it under its declared type: call the callbacks it is or holds (with unknown,
  /// service-free arguments, as builtin callbacks are run), provide or read a key handed over,
  /// and read the members the type declares (a service passed as `FileSystem` has those read).
  /// An `unknown`/`any` position does none of this. A record that can reach no callable waits
  /// until it may: it has no callbacks, and a read of its members demands nothing of a service.
  fn typed_concrete(&mut self, obj: ObjId, ty: u32, site: Site) {
    let t = if ty == u32::MAX { None } else { Some(self.tys[ty as usize].clone()) };
    if t.as_ref().is_some_and(|t| t.ignored && t.prim_only()) {
      return;
    }
    if matches!(self.objs[obj].kind, ObjKind::Object | ObjKind::Array)
      && !self.concrete_seen.contains(&(obj, ty))
      && !self.reaches_code(obj)
    {
      self.deferred.push(Deferred::Concrete(obj, ty, site));
      return;
    }
    if !self.concrete_seen.insert((obj, ty)) {
      return;
    }
    let args = match self.concrete_args {
      Some(v) => v,
      None => {
        let v = self.var();
        self.seed(v, self.top_host);
        self.concrete_args = Some(v);
        v
      }
    };
    let _ = site;
    let call_site = self.typed_callback_site();
    // Only functions run: effect code calls the handlers a value declares, and a summary among
    // them is effect code already (its own calls are modelled where it was made).
    let call_with_unknowns = |a: &mut Self, callee: VarId| {
      let ret = a.discarded();
      let call = Call {
        site: call_site,
        this: None,
        args: vec![Arg::Var(args); 4],
        ret,
        is_new: false,
        builtin: None,
      };
      a.constrain(callee, Constraint::CallFn(Box::new(call)));
    };
    match self.objs[obj].kind.clone() {
      ObjKind::Closure(_) | ObjKind::Builtin { .. } => {
        let holder = self.holder(obj);
        call_with_unknowns(self, holder);
      }
      ObjKind::Key { strings, .. } => self.constrain(strings, Constraint::BailKeys),
      ObjKind::Class { .. } => {
        let strings = self.var();
        self.key_use(obj, &KeyUse::Strings(strings));
        self.constrain(strings, Constraint::BailKeys);
      }
      ObjKind::Object | ObjKind::Array => {
        let holder = self.holder(obj);
        let members: Vec<ArcStr> = t
          .as_ref()
          .map(|t| t.members.iter().flatten().map(|m| ArcStr::from(m.as_str())).collect())
          .unwrap_or_default();
        let fields = if members.is_empty() {
          // No declared members to go by: every member may be read.
          self.record_fields(obj)
        } else {
          let fields = self.var();
          for m in members {
            self.constrain(holder, Constraint::Load { key: Key::Name(m), dst: fields });
          }
          fields
        };
        if matches!(self.objs[obj].kind, ObjKind::Array) {
          let elem = self.elem(obj);
          self.constrain(elem, Constraint::TypedConcrete(u32::MAX, site));
        }
        call_with_unknowns(self, fields);
      }
      _ => {}
    }
  }

  /// The site type-modelled code calls the callbacks it is handed from. Like a wrap's
  /// (`wrap_sites`), it gives each function one clone for all such calls: a clone per call site
  /// repeats a widely used callback's body for every call that hands it over.
  fn typed_callback_site(&mut self) -> Site {
    let site = protocol_site(40);
    self.wrap_sites.insert(site);
    site
  }

  /// A callback handed to type-modelled code. It runs as code the pass can't see would run it
  /// (`escape` with `Flavour::Ext`): once for its function, with unknown arguments, and what it
  /// returns goes to that code too. A service the call would hand one of its parameters of a
  /// type parameter's type (`mask`, from `Dts::callback_masks`) therefore bails, and so does a
  /// service it returns; a parameter of a concrete type receives nothing the call holds. Running
  /// callbacks with the slot instead (`ROLLDOWN_EFFECT_SERVICE_SHAKING_TRACKED_CALLBACKS=1`, a
  /// diagnostic) merges every call's slot into every callback and does not finish on the gates.
  ///
  /// A callback declared to return a value that holds no service (`prim`; `Predicate<A>`, or
  /// `Schedule.map`'s output under assumption 4: `Dts::callback_free_returns`) hands effect's
  /// code nothing it returns: a value at a position whose declared type admits only primitives
  /// holds no service (direction 2, as for arguments at concrete positions). Its return goes to
  /// that code only if the same function is also handed over where its return is open.
  fn typed_callback(
    &mut self,
    f: ObjId,
    slot: VarId,
    back: VarId,
    site: Site,
    mask: u8,
    prim: bool,
  ) {
    if self.debug {
      self.callback_log.push((site.module.raw(), site.start, mask, slot, f));
    }
    let mask = self.declared_args_mask(f, mask);
    if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_TRACKED_CALLBACKS").is_none() {
      if mask != 0 {
        self.constrain(slot, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
      }
      let closure = match self.objs[f].kind {
        ObjKind::Closure(fid) => Some(fid),
        _ => None,
      };
      self.escape_with(f, Flavour::Ext, Some(EFFECT_ESCAPE), closure.is_none());
      // What it returns goes back to the call that runs it, as a value the call holds: it
      // enters the slot as an argument would (an Effect or promise contributes what it succeeds
      // with), so a decoded value that is a service reaches the code reading the result.
      if let (Some(fid), false) = (closure, prim) {
        let ret = self.fns[fid].ret;
        self.constrain(ret, Constraint::SlotIn(slot, self.top_effect));
      }
      let _ = (back, site);
      return;
    }
    // The callback's own body runs (no clone per call site): its parameters receive what the
    // slot holds, and what it returns joins the slot.
    if let ObjKind::Closure(fid) = self.objs[f].kind {
      if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_CLONED_CALLBACKS").is_none() {
        self.activate(fid);
        let (params, rest, this, ret) = {
          let i = &self.fns[fid];
          (i.params.clone(), i.rest, i.this, i.ret)
        };
        // A parameter of a concrete declared type gets no value the call was handed.
        for (j, p) in params.into_iter().enumerate() {
          if mask & (1 << j.min(7)) != 0 {
            self.copy(slot, p);
          } else {
            self.seed(p, self.top_host);
          }
        }
        if let Some(r) = rest {
          let arr = self.rest_array(fid);
          let elem = self.elem(arr);
          if mask & 0x80 != 0 {
            self.copy(slot, elem);
          } else {
            self.seed(elem, self.top_host);
          }
          self.seed(r, arr);
        }
        self.seed(this, self.top_host);
        self.constrain(ret, Constraint::TypedArg(slot, back, site, 0xFF, false, false));
        return;
      }
    }
    let callee = self.holder(f);
    let ret = self.var();
    let call_site = self.typed_callback_site();
    let unknown = self.var();
    self.seed(unknown, self.top_host);
    let args =
      (0..4).map(|j| Arg::Var(if mask & (1 << j) != 0 { slot } else { unknown })).collect();
    let call = Call { site: call_site, this: None, args, ret, is_new: false, builtin: None };
    self.constrain(callee, Constraint::Call(Arc::new(call)));
    self.constrain(ret, Constraint::TypedArg(slot, back, site, 0xFF, false, false));
  }

  /// `mask` (a callback's parameter positions, `Dts::callback_masks`) narrowed to the
  /// arguments `f` can read. An arrow has no `arguments` object of its own, so it reads only
  /// what its declared parameters receive: `() => x` receives nothing the call holds. A rest
  /// parameter receives every argument from its position on (`(...xs) => xs[0]` reads the
  /// first).
  fn declared_args_mask(&self, f: ObjId, mask: u8) -> u8 {
    let ObjKind::Closure(fid) = self.objs[f].kind else { return mask };
    let info = &self.fns[fid];
    if !matches!(info.kind, FnKind::Arrow) {
      return mask;
    }
    let n = info.params.len();
    let mut readable = 0u8;
    for j in 0..n {
      readable |= 1 << j.min(7);
    }
    if info.rest.is_some() {
      readable |= 0xFF << n.min(7);
    }
    mask & readable
  }

  /// The callbacks inside an options object or array handed to type-modelled code.
  fn typed_nested_callbacks(&mut self, obj: ObjId, slot: VarId, back: VarId, site: Site, mask: u8) {
    // Effect's code reads the handlers off the object by name, so every member counts as read
    // (an ordinary unknown-key load): an object whose methods are services' keeps them.
    let fields = self.record_fields(obj);
    self.constrain(fields, Constraint::TypedNested(slot, back, site, mask));
  }

  /// Everything a record or array holds, read by an unknown name: one variable per object,
  /// shared by every type-modelled call it is handed to.
  fn record_fields(&mut self, obj: ObjId) -> VarId {
    if let Some(v) = self.record_fields.get(&obj) {
      return *v;
    }
    let fields = self.var();
    self.record_fields.insert(obj, fields);
    let holder = self.holder(obj);
    self.constrain(holder, Constraint::Load { key: Key::Unknown, dst: fields });
    fields
  }

  /// Where the results of callbacks nothing reads go: one variable for the whole program.
  fn discarded(&mut self) -> VarId {
    if let Some(v) = self.discarded {
      return v;
    }
    let v = self.var();
    self.discarded = Some(v);
    v
  }

  /// `Constraint::BailKeys`: the key the string names keeps every method.
  fn bail_keys(&mut self, obj: ObjId) {
    match &self.objs[obj].kind {
      ObjKind::Str(name) => {
        let name = name.clone();
        self.never_prune.insert(name);
      }
      ObjKind::AllKeys if obj == self.unseen_keys => {}
      ObjKind::AllKeys | ObjKind::Top(_) | ObjKind::External { .. } => {
        self.bail_everything("a key of unknown name is handed to type-modelled effect code");
      }
      _ => {}
    }
  }

  /// `Constraint::ProtocolCall`: a summary under a protocol name hands the receiver to unseen
  /// code; anything else is called.
  fn protocol_method_call(&mut self, obj: ObjId, call: &Call) {
    if matches!(self.objs[obj].kind, ObjKind::Typed { .. }) {
      // Its module's code runs: a consumer for the constructor-key check. What it returns is
      // opaque.
      if let Some(m) = self.typed_module(obj) {
        self.typed_called_modules.insert(m);
      }
      if let Some(this) = call.this {
        self.constrain(this, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
      }
      self.seed(call.ret, self.top_effect);
    } else if let ObjKind::Builtin { rule, .. } = self.objs[obj].kind
      && INERT_PROTOCOL_METHODS.contains(&self.rules[rule as usize].0)
    {
      // A host method that runs no program code: its result is a primitive.
      self.seed(call.ret, self.prim);
    } else {
      self.call(obj, call);
    }
  }

  /// `Constraint::CallFn`: a call that reaches only functions.
  fn call_fn_only(&mut self, obj: ObjId, call: &Call) {
    if Self::function_kind(&self.objs[obj].kind) {
      self.call(obj, call);
    }
  }

  fn function_kind(kind: &ObjKind) -> bool {
    matches!(kind, ObjKind::Closure(_) | ObjKind::Class { .. } | ObjKind::Builtin { .. })
  }

  /// One member of an options object handed to type-modelled code (`Constraint::TypedNested`).
  fn typed_nested(&mut self, obj: ObjId, slot: VarId, back: VarId, site: Site, mask: u8) {
    if Self::function_kind(&self.objs[obj].kind) {
      if self.typed_slot_fns.insert((obj, slot)) {
        self.typed_callback(obj, slot, back, site, mask, false);
      }
    } else if matches!(self.objs[obj].kind, ObjKind::Typed { .. }) && !self.is_stand_in(obj) {
      self.pool_route(obj, back);
    }
  }

  /// A data-last function a type-modelled call is handed (`Queue.offer(q)` as a callback): if
  /// its export may store into a container, what the call is handed goes to that module's pool.
  fn pool_route(&mut self, obj: ObjId, back: VarId) {
    let Some(m) = self.typed_module(obj) else { return };
    // Only a value whose export is known can be told to be a function that stores; the rest
    // (a schema, a request) are data, which effect code at a type parameter's position can't
    // call.
    let stores = self
      .typed_result_origin
      .get(&obj)
      .copied()
      .and_then(|(e, n)| self.typed_sig(e, n))
      .is_some_and(|s| s.stores);
    if stores {
      let pool = self.typed_pool(m);
      self.copy(back, pool);
    }
  }

  /// A value that may be called: a function, class or builtin, or a value that may stand for one.
  fn callable_kind(kind: &ObjKind) -> bool {
    matches!(
      kind,
      ObjKind::Closure(_)
        | ObjKind::Class { .. }
        | ObjKind::Builtin { .. }
        | ObjKind::Typed { .. }
        | ObjKind::Wrap(_)
        | ObjKind::Top(_)
        | ObjKind::External { .. }
    )
  }

  /// A variable holding a primitive, for arguments that carry nothing.
  fn prim_arg(&mut self) -> VarId {
    if let Some(v) = self.prim_arg {
      return v;
    }
    let v = self.var();
    self.seed(v, self.prim);
    self.prim_arg = Some(v);
    v
  }

  /// Whether a value of the declared type `ty` may be this abstract object (Effect's public
  /// types bound value flow, design section 8). Anything the object could still turn into
  /// counts: an object with unknown stores, spread sources, accessors or an escape passes a
  /// member check.
  fn admits(&self, ty: u32, obj: ObjId) -> bool {
    let t = &self.tys[ty as usize];
    if t.open || t.any_object {
      return true;
    }
    let objects = t.data_record || !t.members.is_empty();
    match &self.objs[obj].kind {
      ObjKind::Str(_) | ObjKind::Num(_) | ObjKind::Prim => true,
      // A function passes an object type only if it has the members (a function with fields).
      ObjKind::Closure(_) => {
        t.func || t.members.iter().any(|alt| alt.iter().all(|m| self.has_member(obj, m, 0)))
      }
      ObjKind::Builtin { .. } | ObjKind::Class { .. } | ObjKind::Key { .. } => {
        t.func || !t.members.is_empty()
      }
      ObjKind::Array => t.arrays || !t.members.is_empty(),
      ObjKind::Object => {
        if !objects {
          return false;
        }
        let o = &self.objs[obj];
        if !o.unknown_stores.is_empty() || !o.spread_from.is_empty() || o.escaped.is_some() {
          return true;
        }
        // A record of primitives: a field holding a summary may hold a primitive too.
        if t.data_record
          && !o.fields.values().any(|f| {
            self.vars[*f].pts.iter().any(|x| {
              !matches!(
                self.objs[*x].kind,
                ObjKind::Str(_)
                  | ObjKind::Num(_)
                  | ObjKind::Prim
                  | ObjKind::Wrap(_)
                  | ObjKind::Top(_)
                  | ObjKind::External { .. }
              )
            })
          })
        {
          return true;
        }
        t.members.iter().any(|alt| alt.iter().all(|m| self.has_member(obj, m, 0)))
      }
      // A summary may be a value of any type that admits more than primitives.
      ObjKind::Wrap(_) | ObjKind::Typed { .. } | ObjKind::Top(_) | ObjKind::External { .. } => {
        t.func || t.arrays || objects
      }
      // Effect's objects are objects: only an object-admitting type can hold one.
      _ => objects,
    }
  }

  /// Whether the object may have the member: an own field, a field on its prototype chain, an
  /// accessor declared on it or an ancestor, an unknown store, a spread source (`Object.assign`,
  /// `{ ...x }`) or an escape, on the object or anywhere up its chain.
  fn has_member(&self, obj: ObjId, name: &str, depth: u32) -> bool {
    if depth > 6 {
      return true;
    }
    let o = &self.objs[obj];
    if o.fields.contains_key(name)
      || !o.unknown_stores.is_empty()
      || !o.spread_from.is_empty()
      || o.escaped.is_some()
      || self.accessors.contains_key(&obj)
    {
      return true;
    }
    match o.proto {
      Some(pv) => self.vars[pv].pts.iter().any(|p| match &self.objs[*p].kind {
        ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } | ObjKind::Closure(_) => {
          self.has_member(*p, name, depth + 1)
        }
        _ => true,
      }),
      None => false,
    }
  }

  /// `obj` flows into `dst` if the declared type `ty` admits it; otherwise it waits in
  /// `rejected` until a change to its members admits it (`recheck`).
  fn filter(&mut self, obj: ObjId, ty: u32, dst: VarId) {
    if self.admits(ty, obj) {
      self.seed(dst, obj);
    } else {
      let waiting = self.rejected.entry(obj).or_default();
      if !waiting.contains(&(ty, dst)) {
        waiting.push((ty, dst));
      }
    }
  }

  /// Re-checks the edges waiting for `obj`; returns whether any admitted it.
  fn recheck(&mut self, obj: ObjId) -> bool {
    let Some(waiting) = self.rejected.remove(&obj) else { return false };
    let mut admitted = false;
    let mut keep = vec![];
    for (ty, dst) in waiting {
      if self.admits(ty, obj) {
        self.seed(dst, obj);
        admitted = true;
      } else {
        keep.push((ty, dst));
      }
    }
    if !keep.is_empty() {
      self.rejected.insert(obj, keep);
    }
    admitted
  }

  /// Re-checks `obj` and every object inheriting from it, after a change to its members.
  /// (`heirs` lists every descendant of an object, not only its direct heirs.)
  fn recheck_tree(&mut self, obj: ObjId) {
    if self.rejected.is_empty() {
      return;
    }
    self.recheck(obj);
    let waiting: Vec<ObjId> = match self.heirs.get(&obj) {
      Some(heirs) => heirs.iter().copied().filter(|h| self.rejected.contains_key(h)).collect(),
      None => return,
    };
    for h in waiting {
      self.recheck(h);
    }
  }

  fn recheck_all(&mut self) -> bool {
    let objs: Vec<ObjId> = self.rejected.keys().copied().collect();
    let mut admitted = false;
    for o in objs {
      admitted |= self.recheck(o);
    }
    admitted
  }

  pub fn call_fn(&mut self, f: FnId, call: &Call, instance: Option<ObjId>) -> FnId {
    let f = if instance.is_none() { self.clone_for(f, call) } else { f };
    self.activate(f);
    let (params, rest, this_var, ret, kind, arguments_elem) = {
      let info = &self.fns[f];
      (info.params.clone(), info.rest, info.this, info.ret, info.kind, info.arguments_elem)
    };
    let bounds = self.bounds_of(f);
    let param_ty = |i: usize| bounds.as_ref().and_then(|b| b.params.get(i).copied().flatten());
    let rest_ty = bounds.as_ref().and_then(|b| b.rest);
    if let Some(i) = instance {
      self.seed(this_var, i);
    } else if let Some(this) = call.this {
      self.copy(this, this_var);
    } else if !matches!(kind, FnKind::Arrow) {
      self.seed(this_var, self.top_host);
    }
    if call.has_spread() {
      // Positions are unknown: every parameter may receive any argument or element. One
      // variable per call, shared by every callee, keeps the constraint set finite.
      let any = match self.spread_any.get(&(call.site, call.ret)) {
        Some(any) => *any,
        None => {
          let any = self.var();
          self.spread_any.insert((call.site, call.ret), any);
          for a in &call.args {
            match a {
              Arg::Var(v) => self.copy(*v, any),
              Arg::Spread(v) => self.constrain(*v, Constraint::ElemOf(any)),
            }
          }
          any
        }
      };
      for (i, p) in params.iter().enumerate() {
        self.bounded_copy(any, *p, param_ty(i));
      }
      if let Some(r) = rest {
        let arr = self.rest_array(f);
        let elem = self.elem(arr);
        self.bounded_copy(any, elem, rest_ty);
        self.seed(r, arr);
      }
      self.copy(any, arguments_elem);
    } else {
      for (i, p) in params.iter().enumerate() {
        if let Some(Arg::Var(v)) = call.args.get(i) {
          self.bounded_copy(*v, *p, param_ty(i));
        }
      }
      if let Some(r) = rest {
        let arr = self.rest_array(f);
        let elem = self.elem(arr);
        for a in call.args.iter().skip(params.len()) {
          if let Arg::Var(v) = a {
            self.bounded_copy(*v, elem, rest_ty);
          }
        }
        self.seed(r, arr);
      }
      for a in &call.args {
        if let Arg::Var(v) = a {
          self.copy(*v, arguments_elem);
        }
      }
    }
    match kind {
      FnKind::Normal | FnKind::Arrow => {
        if instance.is_none() {
          let ret_ty = bounds.as_ref().and_then(|b| b.ret);
          self.bounded_copy(ret, call.ret, ret_ty);
        }
      }
      FnKind::Async => {
        let p = self.site_obj(
          Site { module: self.fns[f].module, start: self.fns[f].span_start, sub: 0 },
          202,
          |a| {
            let value = a.var();
            ObjKind::Promise { value }
          },
        );
        let ObjKind::Promise { value } = self.objs[p].kind else { unreachable!() };
        // Returning an object from an async function resolves it, adopting a thenable
        // (`AwaitOf`).
        self.constrain(ret, Constraint::AwaitOf(value));
        self.seed(call.ret, p);
      }
      FnKind::Generator | FnKind::AsyncGenerator => {
        let it = self.site_obj(
          Site { module: self.fns[f].module, start: self.fns[f].span_start, sub: 0 },
          203,
          |_| ObjKind::Iterator(f),
        );
        self.seed(call.ret, it);
      }
    }
    f
  }

  pub fn escape_args(&mut self, call: &Call, flavour: Flavour) {
    for a in &call.args {
      let v = match a {
        Arg::Var(v) | Arg::Spread(v) => *v,
      };
      self.constrain(v, Constraint::Escape(flavour, Some(call.site)));
    }
    if let Some(this) = call.this {
      let c = match self.method_names.get(&call.site) {
        Some(name) => Constraint::EscapeReceiver(flavour, Some(call.site), name.clone()),
        None => Constraint::Escape(flavour, Some(call.site)),
      };
      self.constrain(this, c);
    }
  }

  /// `o.name(...)` called code the pass can't see. The receiver variable holds every object `o`
  /// may be, but unseen code runs with `this` only for the receivers whose `name` it is: an
  /// object whose own `name` slot holds bundled functions only calls those. So a plain
  /// receiver is handed over when its own slot can hold unseen code, or when it has no own slot
  /// (the method comes from its prototype or elsewhere). Other receivers are handed over as
  /// before.
  fn escape_receiver(&mut self, obj: ObjId, flavour: Flavour, site: Option<Site>, name: &ArcStr) {
    match self.objs[obj].kind {
      // A function effect's code made, called as a summary's method: the summary's own methods
      // are its method summary (`typed_member`), whose call decides what it hands over
      // (`typed_call`); a function it holds as a value of a type parameter does not read it
      // through `this`.
      ObjKind::Typed { .. } if site == Some(EFFECT_ESCAPE) && !self.is_stand_in(obj) => {}
      ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. } => {
        match self.objs[obj].fields.get(name).copied() {
          Some(slot) => self.constrain(slot, Constraint::EscapeIfOpaque(obj, flavour, site)),
          None => self.escape(obj, flavour, site),
        }
      }
      _ => self.escape(obj, flavour, site),
    }
  }

  /// The wrap of `inner`, made by the rule call at `site` the first time.
  /// A module namespace is read by `name`, or used wholesale (`None`: an unknown name,
  /// enumeration, spread, contents, escape): the lazy statements behind what it may yield run.
  fn touch_namespace(&mut self, obj: ObjId, name: Option<&ArcStr>) {
    if !matches!(self.objs[obj].kind, ObjKind::Namespace) {
      return;
    }
    match name {
      Some(n) => {
        if let Some(l) = self.lazy_exports.get(&(obj, n.clone())).copied() {
          self.activate(l);
        }
      }
      None => {
        if let Some(ls) = self.lazy_namespaces.remove(&obj) {
          for l in ls {
            self.activate(l);
          }
        }
      }
    }
  }

  /// `Context.get(ctx, key)` with `obj` among `ctx`'s values: only a Context, or a value that
  /// may be one (unknown, or wrapped), hands out the key's services. Anything else reached
  /// where a Context is expected (a MemoMap's or a Context's non-public slot, `mapUnsafe`) is
  /// not a Context by the public types, so the lookup reads nothing (issue, "Assumptions" 2).
  fn lookup_in(&mut self, obj: ObjId, key: VarId, dst: VarId) {
    if matches!(
      self.objs[obj].kind,
      ObjKind::Context { .. }
        | ObjKind::Top(_)
        | ObjKind::External { .. }
        | ObjKind::Wrap(_)
        | ObjKind::Typed { .. }
    ) {
      self.constrain(key, Constraint::KeyUse(KeyUse::Lookup(dst)));
    }
  }

  /// Marks a model Effect as an Exit (see `exits`).
  pub fn mark_exit(&mut self, obj: ObjId) {
    self.exits.insert(obj);
  }

  pub fn wrap_of(&mut self, inner: VarId, site: Site) -> VarId {
    let w = match self.wraps.get(&inner) {
      Some(w) => *w,
      None => {
        let w = self.alloc(ObjKind::Wrap(inner), None);
        self.wraps.insert(inner, w);
        // Callables handed to the wrap run with the wrapped values and feed them back. The
        // calls are made on behalf of the rule call that made the wrap, so they sit on a
        // sub-site of it: on one shared site, every builtin callback in the bundle would share
        // one rule invocation (`rule_memo`) and so one result. Closures still share one clone
        // per function (see `clone_for`).
        let cb = self.var();
        let wrapped = self.holder(w);
        let site = site.sub(cb);
        self.wrap_sites.insert(site);
        let call = Call {
          site,
          this: None,
          args: vec![Arg::Spread(wrapped)],
          ret: inner,
          is_new: false,
          builtin: None,
        };
        self.constrain(cb, Constraint::Call(Arc::new(call)));
        self.wrap_callbacks.insert(w, cb);
        w
      }
    };
    self.holder(w)
  }

  pub fn top_of(&self, obj: ObjId) -> ObjId {
    if self.opaque_class.contains_key(&obj) || obj == self.top_effect {
      return obj;
    }
    match self.objs[obj].kind {
      ObjKind::Top(Flavour::Ext) | ObjKind::External { flavour: Flavour::Ext, .. } => self.top_ext,
      _ => self.top_host,
    }
  }

  pub fn success_of(&mut self, obj: ObjId, dst: VarId) {
    if self.is_default(obj) {
      // A default-modelled effect value run as an effect: it may yield any service of an
      // effect-declared key, or host data.
      self.copy(self.effect_key_values, dst);
      self.seed(dst, self.top_host);
      return;
    }
    match self.objs[obj].kind.clone() {
      ObjKind::Effect { success, .. } => self.copy(success, dst),
      ObjKind::Key { .. } | ObjKind::Class { .. } => self.key_use(obj, &KeyUse::Lookup(dst)),
      ObjKind::Iterator(f) => {
        self.activate(f);
        let ret = self.fns[f].ret;
        self.copy(ret, dst);
      }
      ObjKind::Promise { value } => self.copy(value, dst),
      ObjKind::Wrap(_) => self.seed(dst, obj),
      // Run as an effect, a type-modelled value yields what it holds.
      // A stand-in's elements are values it stands for.
      ObjKind::Typed { .. } if self.is_stand_in(obj) => self.seed(dst, obj),
      ObjKind::Typed { slot, .. } => self.copy(slot, dst),
      ObjKind::Top(Flavour::Ext) | ObjKind::External { flavour: Flavour::Ext, .. } => {
        // An effect built by code the pass can't see runs in this runtime. It reaches services
        // only through the Context it runs with, which is not a path (issue, "Code the pass
        // can't see"): nothing bails, and what it yields is unknown.
        let t = self.top_of(obj);
        self.seed(dst, t);
      }
      ObjKind::Object | ObjKind::Array => {
        // A user object used as an effect (Effectable): what it yields is what its fields hold,
        // at worst. Effect's combinators take an `Effect`, whose type requires the
        // `~effect/Effect` member, so an object that can't have it is not run: it waits like any
        // value a declared type rejects (design section 8).
        let ty = self.effect_type();
        let gate = match self.effect_gates.get(&dst) {
          Some(g) => *g,
          None => {
            let g = self.var();
            self.constrain(g, Constraint::Load { key: Key::Unknown, dst });
            self.effect_gates.insert(dst, g);
            g
          }
        };
        self.filter(obj, ty, gate);
      }
      _ => {}
    }
  }

  /// The declared type `Effect`: an object with the `~effect/Effect` member (its TypeId).
  fn effect_type(&mut self) -> u32 {
    if let Some(t) = self.effect_ty {
      return t;
    }
    self.tys.push(super::dts::Ty {
      members: vec![vec![EFFECT_TYPE_ID.to_string()]],
      ..super::dts::Ty::default()
    });
    let t = u32::try_from(self.tys.len() - 1).expect("too many declared types");
    self.effect_ty = Some(t);
    t
  }

  /// What an effect fails with, as handlers see it. Services never enter the error channel
  /// (issue, "Assumptions" 3), and no `Done` signal carries one (4), so the pass does not track
  /// failures, defects, Causes or `Done` values: handlers still run, with no failure input.
  fn failure_of(&mut self, obj: ObjId, _dst: VarId) {
    if let ObjKind::Iterator(f) = self.objs[obj].kind {
      self.activate(f);
    }
  }

  pub fn key_use(&mut self, obj: ObjId, use_: &KeyUse) {
    match self.objs[obj].kind.clone() {
      ObjKind::Key { strings, .. } => self.constrain(strings, Constraint::KeyNameUse(use_.clone())),
      ObjKind::Class { heritage, .. } => {
        // A class is a key only through its heritage (`key_heritage`): a plain class with a
        // `key` field names no service.
        if let Some(h) = heritage {
          self.constrain(h, Constraint::KeyHeritage(obj, use_.clone()));
        }
      }
      ObjKind::Str(name) => match use_ {
        KeyUse::Lookup(dst) => {
          let v = self.key_value_var(&name);
          self.copy(v, *dst);
        }
        KeyUse::Provide(src) => {
          let v = self.key_value_var(&name);
          self.copy(*src, v);
        }
        KeyUse::Strings(dst) => self.seed(*dst, obj),
      },
      // A key unseen code made names no key the program named (unseen code does not reach the
      // program's services through the runtime): a lookup through it finds what was provided
      // under keys of unknown name, or under effect's own keys.
      ObjKind::AllKeys if obj == self.unseen_keys => match use_ {
        KeyUse::Lookup(dst) => {
          self.copy(self.unknown_key_values, *dst);
          self.copy(self.effect_key_values, *dst);
          self.handed_lookup(*dst);
        }
        KeyUse::Provide(src) => {
          self.copy(*src, self.unknown_key_values);
        }
        KeyUse::Strings(dst) => self.seed(*dst, obj),
      },
      ObjKind::AllKeys => match use_ {
        KeyUse::Lookup(dst) => self.copy(self.all_values, *dst),
        KeyUse::Provide(src) => {
          self.copy(*src, self.unknown_key_values);
        }
        KeyUse::Strings(dst) => self.seed(*dst, obj),
      },
      // A key is read through its `key` property (a hand-made `{ key: "..." }` works at run
      // time): a record, array or function names the keys its `key` field holds.
      ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Namespace => {
        let name = ArcStr::from("key");
        // A namespace's `key` export is read: the statement declaring it runs (`lazy_exports`).
        self.touch_namespace(obj, Some(&name));
        let key = self.field(obj, &name);
        self.constrain(key, Constraint::KeyNameUse(use_.clone()));
      }
      ObjKind::Wrap(inner) => self.constrain(inner, Constraint::KeyUse(use_.clone())),
      // A key effect's code made (a summary, or `top_effect`): a key factory such as
      // `HttpApiMiddleware.Service()("id")` takes its name from the program, so the name is
      // unknown here, and it is read as a key made where the pass can't see. What is provided
      // through it is kept whole.
      // (Read by the last arm, with `top_effect`.)
      // A default-modelled export names one of effect's keys.
      ObjKind::Builtin { .. } if self.is_default(obj) => {
        self.constrain(self.effect_key_strings, Constraint::KeyUse(use_.clone()));
      }
      // Effects, promises, containers, builtins and primitives have no `key` of their own: a
      // key is an object (by the public types). A primitive as a key's *name* is an unknown
      // name (`key_name_use`).
      ObjKind::Effect { .. }
      | ObjKind::Promise { .. }
      | ObjKind::Iterator(_)
      | ObjKind::Context { .. }
      | ObjKind::Layer { .. }
      | ObjKind::Runtime { .. }
      | ObjKind::Fiber
      | ObjKind::MemoMap
      | ObjKind::Builtin { .. }
      | ObjKind::Num(_)
      | ObjKind::Prim => {}
      // A value from a boundary in a key position (code the pass can't see, effect's code, a
      // host builtin, class code or a widened set): a key made there, or one handed to it
      // (`hand_key`).
      ObjKind::Typed { .. } | ObjKind::Top(_) | ObjKind::External { .. } => {
        self.boundary_key(obj);
        let unseen = self.unseen_keys;
        self.key_use(unseen, use_);
      }
    }
  }

  /// A boundary's value in a key position. Reading it as a key unseen code made assumes that
  /// code never names the program's keys; a module hole is bundled code that may, so it is
  /// recorded (`keyed_boundaries`) and analysed in the next solve.
  fn boundary_key(&mut self, obj: ObjId) {
    if let Some(&class) = self.opaque_class.get(&obj) {
      self.keyed_boundaries.insert(class);
    }
  }

  /// `class` extends `base`, an object reaching its heritage variable. The class is a service key
  /// only when `base` is a key object: it directly extends what `Context.Service` or
  /// `Context.Reference` returns. Heritage grows during the solve, so this runs as a constraint on
  /// each heritage variable and fires for every object that arrives there later. A class whose
  /// heritage is code the pass can't see may be a key too, and keeps the old reading.
  ///
  /// A subclass of another class is not a key, even when that class is one: the rule covers only
  /// the direct form (issue, "Analysis model"). Constructors such as `HttpApiMiddleware.Service`
  /// return such a subclass, and their services must not be pruned, so a class that reaches a key
  /// through another class is read as an unknown key instead (`subclass_heritage`). See
  /// internal-docs/effect-service-shaking/implementation.md.
  ///
  /// A key class's own `static key` field overrides the inherited string, so it is read too.
  fn key_heritage(&mut self, base: ObjId, class: ObjId, use_: &KeyUse) {
    match self.objs[base].kind.clone() {
      ObjKind::Key { strings, .. } => {
        self.constrain(strings, Constraint::KeyNameUse(use_.clone()));
        self.own_key_use(class, use_);
      }
      ObjKind::Class { heritage: Some(h), .. } => {
        self.constrain(h, Constraint::SubclassHeritage(use_.clone()));
      }
      // A class extending a type-modelled value may extend a key effect's code made from a
      // name the program passed (`HttpApiMiddleware.Service()("id")`): a key of unknown name,
      // read as one made where the pass can't see.
      ObjKind::Typed { .. } => {
        let unseen = self.unseen_keys;
        self.key_use(unseen, use_);
      }
      ObjKind::Top(_) | ObjKind::Wrap(_)
        if base == self.top_effect || matches!(self.objs[base].kind, ObjKind::Wrap(_)) =>
      {
        let unseen = self.unseen_keys;
        self.key_use(unseen, use_);
      }
      ObjKind::Top(_) | ObjKind::External { .. } | ObjKind::Wrap(_) => {
        self.boundary_key(base);
        self.key_use(base, use_);
        self.own_key_use(class, use_);
      }
      ObjKind::Builtin { .. } if self.is_default(base) => {
        self.key_use(base, use_);
        self.own_key_use(class, use_);
      }
      // A class without heritage, or one extending anything else, is not a key.
      _ => {}
    }
  }

  /// A class extends another class whose heritage `base` reaches. If that chain reaches a key,
  /// or heritage the pass can't see, the subclass may share a key at run time, and neither the
  /// key nor the service is modelled: the subclass is read as an unknown key. Its providers are
  /// then kept whole (`unknown_key_values`), and a lookup through it sees every service, so the
  /// methods it calls run in the analysis and record their reads. Issue, "Analysis model":
  /// services from constructors other than `Context.Service` and `Context.Reference` are not
  /// pruned.
  fn subclass_heritage(&mut self, base: ObjId, use_: &KeyUse) {
    let possible_key = match self.objs[base].kind.clone() {
      ObjKind::Class { heritage: Some(h), .. } => {
        self.constrain(h, Constraint::SubclassHeritage(use_.clone()));
        false
      }
      ObjKind::Typed { .. } => {
        let unseen = self.unseen_keys;
        self.key_use(unseen, use_);
        false
      }
      // A class extending a value effect's code or a default-modelled export made may share one
      // of effect's keys (effect's code never names a user key).
      ObjKind::Top(_) | ObjKind::Wrap(_)
        if base == self.top_effect || matches!(self.objs[base].kind, ObjKind::Wrap(_)) =>
      {
        let unseen = self.unseen_keys;
        self.key_use(unseen, use_);
        false
      }
      // A subclass of a key class shares that key (the key is inherited): it names that key, not
      // every key.
      ObjKind::Key { .. } => {
        self.key_use(base, use_);
        false
      }
      ObjKind::Top(_) | ObjKind::External { .. } | ObjKind::Wrap(_) => true,
      ObjKind::Builtin { .. } => self.is_default(base),
      _ => false,
    };
    if possible_key {
      self.boundary_key(base);
      // Heritage from code the pass can't see: a key that code may have made.
      let unseen = self.unseen_keys;
      self.key_use(unseen, use_);
    }
  }

  /// A key class's own `key` field (`static key = "..."`) names its key.
  fn own_key_use(&mut self, class: ObjId, use_: &KeyUse) {
    let own = self.field(class, &ArcStr::from("key"));
    self.constrain(own, Constraint::KeyNameUse(use_.clone()));
  }

  /// A key's name (`Constraint::KeyNameUse`): a string names that key; a primitive or host
  /// value a key the program named at run time (any key); a value from code the pass can't see,
  /// a key that code made (`unseen_keys`); one effect's code made, one of effect's keys. Any
  /// other value is no name: effect's maps are keyed by the name string, which a record, array,
  /// function or opaque value never equals.
  fn key_name_use(&mut self, obj: ObjId, use_: &KeyUse) {
    match self.objs[obj].kind {
      ObjKind::Str(_) | ObjKind::AllKeys => self.key_use(obj, use_),
      ObjKind::Top(_) if obj == self.top_effect => {
        self.constrain(self.effect_key_strings, Constraint::KeyUse(use_.clone()));
      }
      ObjKind::Top(Flavour::Host) if self.prefixes.contains_key(&obj) => {
        let prefix = self.prefixes[&obj].clone();
        self.prefix_key_use(&prefix, use_);
      }
      ObjKind::Num(_) | ObjKind::Prim | ObjKind::Top(Flavour::Host) => {
        let all = self.all_keys;
        self.key_use(all, use_);
      }
      ObjKind::Top(_) | ObjKind::External { .. } => {
        self.boundary_key(obj);
        let unseen = self.unseen_keys;
        self.key_use(unseen, use_);
      }
      _ => {}
    }
  }

  /// Iteration: array elements, or the unknown-key slot of a record used as an iterable.
  fn elem_of(&mut self, obj: ObjId, dst: VarId) {
    match self.objs[obj].kind.clone() {
      ObjKind::Array | ObjKind::Object => {
        let elem = self.elem(obj);
        self.copy(elem, dst);
        let sources = self.objs[obj].spread_from.clone();
        for src in sources {
          self.constrain(src, Constraint::ElemOf(dst));
        }
      }
      ObjKind::Str(_) => self.seed(dst, self.top_host),
      ObjKind::Wrap(_) => self.seed(dst, obj),
      // A stand-in's elements are values it stands for.
      ObjKind::Typed { .. } if self.is_stand_in(obj) => self.seed(dst, obj),
      ObjKind::Typed { slot, .. } => self.copy(slot, dst),
      ObjKind::Top(_) | ObjKind::External { .. } => {
        let t = self.top_of(obj);
        self.seed(dst, t);
      }
      ObjKind::Iterator(f) => {
        // Iterating a generator yields what it yields.
        self.activate(f);
        let yielded = self.fns[f].yielded;
        self.copy(yielded, dst);
      }
      // A Context is not iterable by its public type.
      ObjKind::Context { .. } => {}
      _ => {
        let elem = self.elem(obj);
        self.copy(elem, dst);
      }
    }
  }

  /// Everything a value holds, for an API the pass has no model for.
  fn contents_of(&mut self, obj: ObjId, dst: VarId) {
    self.touch_namespace(obj, None);
    match self.objs[obj].kind.clone() {
      ObjKind::Array | ObjKind::Object => {
        self.load_unknown(obj, dst);
      }
      ObjKind::Wrap(_) => self.seed(dst, obj),
      // A stand-in's elements are values it stands for.
      ObjKind::Typed { .. } if self.is_stand_in(obj) => self.seed(dst, obj),
      ObjKind::Typed { slot, .. } => self.copy(slot, dst),
      ObjKind::Top(_) | ObjKind::External { .. } => {
        let t = self.top_of(obj);
        self.seed(dst, t);
      }
      // A container handed to an API without a model: what it may hand back are the services
      // of its keys. Reads on them are then recorded as demand, like any lookup. That code is
      // effect's, which names no user key: through keys it does not know (the ambient
      // Context's), it reaches effect's keys and those provided under names unknown to the pass.
      ObjKind::Context { keys } | ObjKind::Layer { keys } | ObjKind::Runtime { keys } => {
        self.constrain(keys, Constraint::ContentsLookup(dst));
      }
      _ => {}
    }
  }

  /// One object reached by a walk (`Walk`): the walk reads none of its keys, so nothing is
  /// demanded and nothing bails, but every value it holds, now or later, joins the pool and is
  /// walked in turn. The protocol methods the walk calls are loaded by name and called with the
  /// object as `this`, exactly as `obj.toString()` in bundled code would be: they count as
  /// reads and their bodies run. Their results are walked too.
  fn walk(&mut self, obj: ObjId, walk: Walk, pool: VarId, arg: VarId, site: Site) {
    // Direction 2's admission lever: a value that reaches no callable has no protocol method to
    // call and holds no service, so walking it reads nothing. Its walk waits until it may.
    if matches!(self.objs[obj].kind, ObjKind::Object | ObjKind::Array)
      && !self.walked.contains(&(pool, obj))
      && !self.reaches_code(obj)
    {
      self.deferred.push(Deferred::Walk(obj, walk, pool, arg, site));
      return;
    }
    if !self.walked.insert((pool, obj)) {
      return;
    }
    let again = Constraint::Walk { walk, pool, arg, site };
    match self.objs[obj].kind.clone() {
      ObjKind::Object
      | ObjKind::Array
      | ObjKind::Closure(_)
      | ObjKind::Class { .. }
      | ObjKind::Namespace => {
        self.touch_namespace(obj, None);
        let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
        for f in fields {
          self.copy(f, pool);
        }
        // Fields created later reach the pool too (without the demand `load_unknown` adds).
        if !self.objs[obj].unknown_loads.contains(&pool) {
          self.objs[obj].unknown_loads.push(pool);
        }
        let elem = self.elem(obj);
        self.copy(elem, pool);
        for src in self.objs[obj].spread_from.clone() {
          self.constrain(src, again.clone());
        }
        // `Hash`'s and `Equal`'s key walks include inherited keys.
        if let Some(proto) = self.objs[obj].proto {
          self.constrain(proto, again);
        }
        let saved = std::mem::replace(&mut self.var_tag, "walk holder");
        let this = self.holder(obj);
        self.var_tag = saved;
        // The protocol walk passes each method what the host or Effect passes it: the current
        // Context to Redactable, Node's inspector arguments to `[util.inspect.custom]`, and a
        // primitive to the rest.
        let protocol_args =
          if walk == Walk::Protocol { Some((self.prim_arg(), self.inspect_args())) } else { None };
        // A value at an `unknown`/`any` position may be adopted as a thenable (an async function
        // returning what `Inspectable.toJson` hands back): one with a callable `then` bails, as
        // in `adopt`.
        if walk == Walk::Protocol {
          let sink = self.var();
          self.adopt(obj, sink);
        }
        for (name, arity) in walk.protocols() {
          let arg = match protocol_args {
            Some(_) if *name == REDACTABLE_KEY => arg,
            Some((_, inspect)) if *name == super::host::INSPECT_CUSTOM => inspect,
            Some((prim, _)) => prim,
            None => arg,
          };
          let method = self.protocol_call(this, name, *arity, arg, site, pool);
          if walk == Walk::Equal {
            self.constrain(method, Constraint::Witness(obj, arg));
          }
        }
      }
      // Printing an Effect (an Exit, say) prints what it holds; comparing two compares them.
      // Its failures hold no service (issue, "Assumptions" 3).
      ObjKind::Effect { success, .. } => self.constrain(success, again),
      // A wrapped value may be any of the values it wraps. A Promise has no keys of its own.
      ObjKind::Wrap(inner) | ObjKind::Typed { slot: inner, .. } => self.constrain(inner, again),
      // A printed Context or runtime prints its services. Context equality is ignored.
      ObjKind::Context { keys } | ObjKind::Runtime { keys } if walk == Walk::Protocol => {
        let services = self.var();
        self.constrain(keys, Constraint::KeyUse(KeyUse::Lookup(services)));
        self.constrain(services, again);
      }
      // Host printing of Effect's containers runs Effect's own `toJSON`/inspect, which prints
      // the entries' services (as `prune::apply_protocols` assumes); a Promise prints its value.
      ObjKind::Context { .. } | ObjKind::Runtime { .. } | ObjKind::Fiber
        if matches!(walk, Walk::Json | Walk::Inspect) =>
      {
        self.copy(self.all_values, pool);
      }
      ObjKind::Promise { value } if matches!(walk, Walk::Json | Walk::Inspect) => {
        self.constrain(value, again);
      }
      _ => {}
    }
  }

  /// `this.name(arg, ...)` with `arity` copies of `arg`, as a protocol call: the method is
  /// loaded by name (a read) and its result flows to `ret`. Returns the method variable.
  fn protocol_call(
    &mut self,
    this: VarId,
    name: &str,
    arity: usize,
    arg: VarId,
    site: Site,
    ret: VarId,
  ) -> VarId {
    let saved = std::mem::replace(&mut self.var_tag, "walk method");
    let method = self.var();
    self.constrain(this, Constraint::Load { key: Key::Name(ArcStr::from(name)), dst: method });
    self.var_tag = saved;
    // The result goes straight to `ret`: a variable of its own would only relay it there.
    let call = Call {
      site: site.sub(method),
      this: Some(this),
      args: vec![Arg::Var(arg); arity],
      ret,
      is_new: false,
      builtin: None,
    };
    self.constrain(method, Constraint::ProtocolCall(Arc::new(call)));
    method
  }

  /// Host formatting (`Constraint::Protocol`) runs the protocol methods it calls, so their
  /// bodies count; `prune::apply_protocols` also records them as demand. String and number
  /// conversion calls `toString`, `valueOf` and `[Symbol.toPrimitive]` on the value itself.
  /// `JSON.stringify` and `console.*`/`util.inspect` recurse, through the program-wide walks
  /// `Walk::Json` and `Walk::Inspect`.
  fn host_protocol(&mut self, obj: ObjId, protocol: Protocol) {
    match protocol {
      Protocol::Primitive => {
        if !matches!(
          self.objs[obj].kind,
          ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. }
        ) || !self.primitive_converted.insert(obj)
        {
          return;
        }
        let this = self.holder(obj);
        let arg = self.var();
        self.seed(arg, self.prim);
        let ret = self.var();
        let site = protocol_site(1);
        for name in ["toString", "valueOf", "@@toPrimitive"] {
          self.protocol_call(this, name, 1, arg, site, ret);
        }
      }
      Protocol::Json | Protocol::Inspect => {
        let pool = self.protocol_pool(protocol);
        self.seed(pool, obj);
      }
    }
  }

  fn protocol_pool(&mut self, protocol: Protocol) -> VarId {
    let (existing, walk, n) = match protocol {
      Protocol::Json => (self.json_pool, Walk::Json, 2),
      _ => (self.inspect_pool, Walk::Inspect, 3),
    };
    if let Some(pool) = existing {
      return pool;
    }
    let pool = self.var();
    if walk == Walk::Json {
      self.json_pool = Some(pool);
    } else {
      self.inspect_pool = Some(pool);
    }
    // `toJSON(key)`; `[util.inspect.custom](depth, options, inspect)`.
    let arg = self.var();
    self.seed(arg, self.prim);
    if walk == Walk::Inspect {
      self.seed(arg, self.top_host);
      if let Some(id) = self.rule_id("util.inspect") {
        let inspect = self.builtin(id, None, None);
        self.seed(arg, inspect);
      }
    }
    self.constrain(pool, Constraint::Walk { walk, pool, arg, site: protocol_site(n) });
    pool
  }

  pub fn enumerate(&mut self, obj: ObjId) {
    self.touch_namespace(obj, None);
    match self.objs[obj].kind.clone() {
      ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. } => {
        self.objs[obj].demand.all();
        let sources = self.objs[obj].spread_from.clone();
        for src in sources {
          self.constrain(src, Constraint::Enumerate);
        }
      }
      // Containers have no enumerable public data: enumerating one keeps nothing.
      ObjKind::Wrap(inner) => self.constrain(inner, Constraint::Enumerate),
      _ => {}
    }
  }

  fn spread_into(&mut self, obj: ObjId, target: ObjId) {
    self.touch_namespace(obj, None);
    match self.objs[obj].kind {
      ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } | ObjKind::Closure(_) => {
        // Reads on the target reach this object through `spread_from`; also record the
        // edge for demand propagation when pruning.
        let holder = self.holder(obj);
        if !self.objs[target].spread_from.contains(&holder) {
          self.objs[target].spread_from.push(holder);
          self.graph_changed();
          self.recheck_tree(target);
          // A source spread into an object after it crossed into class code crosses too, as
          // the sources it had then did (`class_escape`).
          for class in self.crossed_classes(target) {
            self.constrain(holder, Constraint::ClassEscape(class, CLASS_STORED));
          }
          // And one spread into it after it escaped escapes too, as a value stored on it does.
          if let Some(Flavour::Ext) = self.objs[target].escaped {
            let site = (!self.genuine_escaped.contains(&target)).then_some(EFFECT_ESCAPE);
            self.constrain(holder, Constraint::Escape(Flavour::Ext, site));
          }
        }
        if !self.objs[obj].spread_into.contains(&target) {
          self.objs[obj].spread_into.push(target);
        }
        // Loads already registered on the target must also see this source.
        let loads = self.objs[target].unknown_loads.clone();
        for dst in loads {
          self.load_unknown(obj, dst);
        }
        let mut named: Vec<(ArcStr, VarId)> =
          self.objs[target].fields.iter().map(|(k, v)| (k.clone(), *v)).collect();
        // And the named reads made on the target before this source arrived. Each is a load on
        // the source's holder, as a read made after it is (`load_name`): a field the source
        // gains later reaches it (`field`), and so does what a source spread into this one
        // later holds. Each is an entry as it is queued (`total_replay_loads`), so the budget
        // check before the next one sees it.
        named.extend(self.named_loads(target));
        for (name, dst) in named {
          if self.replay_stopped() {
            break;
          }
          let queued = self.vars[holder].constraints.len();
          self.constrain(holder, Constraint::Load { key: Key::Name(name), dst });
          self.total_replay_loads += (self.vars[holder].constraints.len() - queued) as u64;
        }
      }
      ObjKind::Wrap(inner) | ObjKind::Typed { slot: inner, .. } => {
        let elem = self.elem(target);
        self.copy(inner, elem);
      }
      ObjKind::Top(_) | ObjKind::External { .. } => {
        let elem = self.elem(target);
        let t = self.top_of(obj);
        self.seed(elem, t);
      }
      // Spreading a container copies no public data.
      _ => {}
    }
  }

  /// A class whose body the pass does not analyse (a hole, `generate.rs`): `class` and its
  /// `prototype` object are class-related, and what crosses into them is class code's.
  pub fn hole(&mut self, class: ObjId, prototype: ObjId) {
    self.hole_related.insert(class, class);
    self.hole_related.insert(prototype, class);
    self.opaque(class);
    self.class_escape(class, class, CLASS_STORED);
    self.class_escape(prototype, class, CLASS_STORED);
    // Objects already inheriting from them.
    for o in [class, prototype] {
      if let Some(heirs) = self.heirs.get(&o).cloned() {
        for h in heirs {
          self.hole_related.insert(h, class);
          self.class_escape(h, class, CLASS_STORED);
        }
      }
    }
  }

  /// The one value standing for everything the code of `class` makes: it behaves as a value
  /// from code the pass can't see (`top_ext`), except that what it is handed or given crosses
  /// into the class (`ClassEscape`), and reads and calls on it yield itself.
  pub fn opaque(&mut self, class: ObjId) -> ObjId {
    if let Some(o) = self.opaque_of.get(&class) {
      return *o;
    }
    let site = self.objs[class].site;
    let o =
      self.alloc(ObjKind::External { flavour: Flavour::Ext, name: ArcStr::from("class") }, site);
    self.opaque_of.insert(class, o);
    self.opaque_class.insert(o, class);
    o
  }

  /// The class boundaries `obj` crossed: the first (`class_escaped`), and the widening
  /// boundary if it crossed that one too.
  fn crossed_classes(&self, obj: ObjId) -> Vec<ObjId> {
    let mut out = Vec::new();
    if let Some(&(first, _)) = self.class_escaped.get(&obj) {
      out.push(first);
      if let Some(w) =
        self.widened_class.filter(|w| *w != first && self.widen_escaped.contains(&obj))
      {
        out.push(w);
      }
    }
    out
  }

  /// `Constraint::ClassEscape`: `obj` crosses into the body of `class`. Tim's rule makes class
  /// code a boundary: a service that reaches it keeps every method. It is the `Ext` escape, but
  /// class code runs in this runtime: a key it holds can be yielded (the key bails), a function
  /// it holds runs with its opaque value as arguments and `this`, and an Effect it holds runs.
  /// Each object crosses once, whatever other flavour it escaped with before, except that one
  /// which crossed another class boundary first crosses the widening boundary as well: widening
  /// reads a runtime name as any key's (`hand_key`), so it walks what the object holds, and
  /// will hold, again. Later edges (stores, spread sources) cross into both
  /// (`crossed_classes`).
  pub fn class_escape(&mut self, obj: ObjId, class: ObjId, how: u8) {
    let widening = Some(class) == self.widened_class;
    self.hand_key(obj, widening);
    match self.class_escaped.entry(obj) {
      std::collections::hash_map::Entry::Occupied(_) => {
        if !widening || !self.widen_escaped.insert(obj) {
          return;
        }
      }
      std::collections::hash_map::Entry::Vacant(e) => {
        e.insert((class, how));
        if widening {
          self.widen_escaped.insert(obj);
        }
      }
    }
    let op = self.opaque(class);
    let esc = Constraint::ClassEscape(class, how);
    match self.objs[obj].kind.clone() {
      ObjKind::Object
      | ObjKind::Array
      | ObjKind::Class { .. }
      | ObjKind::Closure(_)
      | ObjKind::Namespace => {
        self.objs[obj].demand.all();
        self.touch_namespace(obj, None);
        let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
        for f in fields {
          self.constrain(f, esc.clone());
        }
        let elem = self.elem(obj);
        self.constrain(elem, esc.clone());
        // Fields class code writes may hold anything it made.
        self.seed(elem, op);
        if !self.objs[obj].unknown_stores.contains(&elem) {
          self.objs[obj].unknown_stores.push(elem);
          self.graph_changed();
        }
        self.recheck_tree(obj);
        for src in self.objs[obj].spread_from.clone() {
          self.constrain(src, esc.clone());
        }
        // What it inherits crosses with it, including a prototype installed later.
        let proto = self.proto_var(obj);
        self.constrain(proto, esc.clone());
        if let ObjKind::Class { prototype, ctor, heritage } = self.objs[obj].kind {
          self.constrain(prototype, esc.clone());
          if let Some(f) = ctor {
            self.class_fn(f, class, op);
          }
          if let Some(h) = heritage {
            self.constrain(h, esc);
          }
        }
        if let ObjKind::Closure(f) = self.objs[obj].kind {
          self.class_fn(f, class, op);
        }
        if let Some(receivers) = self.pending_receivers.get(&obj).cloned() {
          for r in receivers {
            self.constrain(r, Constraint::ClassEscape(class, CLASS_HANDED));
          }
        }
      }
      // Class code can yield a key it holds: the key bails.
      ObjKind::Key { strings, .. } => self.constrain(strings, Constraint::BailKeys),
      // Class code holding effect's key constructor can make keys of any name.
      ObjKind::Builtin { rule, .. } if self.key_constructor(rule) => {
        self.bail_everything("class code holds a key constructor, which can make keys of any name");
      }
      ObjKind::Effect { success, .. } => self.constrain(success, esc),
      ObjKind::Promise { value } | ObjKind::Wrap(value) => self.constrain(value, esc),
      ObjKind::Typed { slot, export } => {
        // A type-modelled function class code holds may run: its module's unseen reads count
        // (the constructor-key check in `mod.rs`).
        if self.typed_callable(export) {
          if let Some(m) = self.typed_module(obj) {
            self.typed_called_modules.insert(m);
          }
        }
        self.constrain(slot, esc);
      }
      ObjKind::Iterator(f) => {
        self.activate(f);
        let ret = self.fns[f].ret;
        self.constrain(ret, esc);
      }
      // Effect's containers are no path to a service without a key, which bails on its own;
      // builtins, primitives and values from unseen code hold nothing of the program's.
      _ => {}
    }
  }

  /// The class-hole constraints (`Constraint::ClassEscape` and the rest).
  fn apply_class(&mut self, obj: ObjId, c: &Constraint) {
    match c {
      Constraint::ClassEscape(class, how) => self.class_escape(obj, *class, *how),
      Constraint::ClassRef(class, path) => self.class_ref(obj, *class, path),
      Constraint::ClassReceiver(receiver) => self.class_receiver(obj, *receiver),
      Constraint::HasInstance(x, site) => self.has_instance(obj, *x, *site),
      _ => {}
    }
  }

  /// A function class code holds: class code may call it, with the class's opaque value as its
  /// arguments and `this`, and what that call returns or yields goes back into the class. The
  /// call is one of its own (a clone per class where the function allows one), so values the
  /// function returns to other callers stay theirs.
  fn class_fn(&mut self, f: FnId, class: ObjId, op: ObjId) {
    let arg = self.var();
    self.seed(arg, op);
    let this = self.var();
    self.seed(this, op);
    let nparams = self.fns[f].params.len();
    let mut args = vec![Arg::Var(arg); nparams];
    args.push(Arg::Spread(self.opaque_array(class, op)));
    let ret = self.var();
    let site = self.objs[class].site.unwrap_or(protocol_site(41));
    let call = Call { site, this: Some(this), args, ret, is_new: false, builtin: None };
    let ran = self.call_fn(f, &call, None);
    self.constrain(ret, Constraint::ClassEscape(class, CLASS_HANDED));
    let yielded = self.fns[ran].yielded;
    self.constrain(yielded, Constraint::ClassEscape(class, CLASS_HANDED));
  }

  /// An array of the class's opaque value, for the arguments class code passes beyond the
  /// declared parameters.
  fn opaque_array(&mut self, class: ObjId, op: ObjId) -> VarId {
    let site = self.objs[class].site.unwrap_or(protocol_site(41));
    let arr = self.site_obj(site, 205, |_| ObjKind::Array);
    let elem = self.elem(arr);
    self.seed(elem, op);
    let v = self.var();
    self.seed(v, arr);
    v
  }

  fn typed_callable(&self, export: u32) -> bool {
    if export == NO_EXPORT {
      return true;
    }
    let e = &self.typed_exports[export as usize];
    self.dts.generic_positions(&e.dts, &e.name, 0).is_some()
  }

  /// `Constraint::ClassRef`: a class body references the binding holding `obj`, through `path`.
  fn class_ref(&mut self, obj: ObjId, class: ObjId, path: &[ArcStr]) {
    match (&self.objs[obj].kind, path.split_first()) {
      (ObjKind::Namespace, Some((name, rest))) => {
        let member = self.var();
        self.load_name(obj, name, member);
        self.constrain(member, Constraint::ClassRef(class, rest.to_vec()));
      }
      _ => self.class_escape(obj, class, CLASS_REFERENCED),
    }
  }

  /// `Constraint::ClassReceiver`: `obj` is a `Reflect.get`/`Reflect.set` target.
  fn class_receiver(&mut self, obj: ObjId, receiver: VarId) {
    match self.hole_related.get(&obj) {
      Some(&class) => self.constrain(receiver, Constraint::ClassEscape(class, CLASS_HANDED)),
      None => self.pending_receivers.entry(obj).or_default().push(receiver),
    }
  }

  /// `Constraint::HasInstance`: `x instanceof obj` runs `obj[Symbol.hasInstance](x)`. On a hole
  /// class that is class code receiving `x`; on code the pass can't see, `x` is handed over; on
  /// a class or function of the program, its own static runs if it has one. Host builtins and
  /// type-modelled values have the default check (their public types declare none), which
  /// only reads the prototype chain.
  fn has_instance(&mut self, obj: ObjId, x: VarId, site: Site) {
    if let Some(&class) = self.hole_related.get(&obj) {
      self.constrain(x, Constraint::ClassEscape(class, CLASS_HANDED));
      return;
    }
    if let Some(&class) = self.opaque_class.get(&obj) {
      self.constrain(x, Constraint::ClassEscape(class, CLASS_HANDED));
      return;
    }
    match self.objs[obj].kind {
      ObjKind::Top(flavour) | ObjKind::External { flavour, .. } => {
        self.constrain(x, Constraint::Escape(flavour, Some(site)));
      }
      ObjKind::Closure(_) | ObjKind::Class { .. } | ObjKind::Object => {
        let method = self.var();
        self.load_name(obj, &ArcStr::from("@@hasInstance"), method);
        let holder = self.holder(obj);
        let call = Call {
          site,
          this: Some(holder),
          args: vec![Arg::Var(x)],
          ret: self.var(),
          is_new: false,
          builtin: None,
        };
        self.constrain(method, Constraint::CallFn(Box::new(call)));
      }
      _ => {}
    }
  }

  /// Tim's class rule (issue, "When a method is kept"): a service that reaches class code, or
  /// a value holding one, bails its key. The class holes enforce it during the solve
  /// (`class_escape`); this reads what they recorded, marks those keys as never pruned and
  /// returns each with how its service crossed into which class, for the report.
  pub fn class_bail(&mut self) -> Vec<(ArcStr, String)> {
    let bailed = self.hole_bails();
    for (k, _) in &bailed {
      self.never_prune.insert(k.clone());
    }
    bailed
  }

  /// The class holes a service, a key or a provider reached during the solve: those behind
  /// `hole_bails`, those holding effect's key constructor, and those whose opaque value reached
  /// a key position (`keyed_boundaries`).
  pub fn bailing_holes(&self) -> FxHashSet<ObjId> {
    let mut out = self.keyed_boundaries.clone();
    for (k, v) in &self.key_values {
      if self.never_prune.contains(k) {
        continue;
      }
      for o in &self.vars[*v].pts {
        let holder = matches!(
          self.objs[*o].kind,
          ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } | ObjKind::Closure(_)
        );
        if let Some(&(c, _)) = self.class_escaped.get(o).filter(|_| holder) {
          out.insert(c);
        } else if let Some(&c) = self.opaque_class.get(o) {
          out.insert(c);
        }
      }
    }
    for (o, (c, _)) in &self.class_escaped {
      match self.objs[*o].kind {
        ObjKind::Key { strings, .. } => {
          if self.vars[strings].pts.iter().any(|st| {
            matches!(&self.objs[*st].kind, ObjKind::Str(name) if self.key_values.contains_key(name))
          }) {
            out.insert(*c);
          }
        }
        ObjKind::Builtin { rule, .. } if self.key_constructor(rule) => {
          out.insert(*c);
        }
        _ => {}
      }
    }
    out
  }

  /// The keys the class holes bailed during the solve, with how each crossed into class code.
  fn hole_bails(&self) -> Vec<(ArcStr, String)> {
    let site = |c: ObjId| self.objs[c].site;
    let mut out: Vec<(ArcStr, String)> = vec![];
    let mut push = |k: &ArcStr, why: String| {
      if !out.iter().any(|(b, _)| b == k) {
        out.push((k.clone(), why));
      }
    };
    let mut keys: Vec<(&ArcStr, &VarId)> = self.key_values.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in keys {
      if self.never_prune.contains(k) {
        continue;
      }
      for o in &self.vars[*v].pts {
        // Only an object of the program's crosses into class code as a service; values from
        // code the pass can't see provide nothing it prunes.
        let holder = matches!(
          self.objs[*o].kind,
          ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } | ObjKind::Closure(_)
        );
        if let Some(&(c, how)) = self.class_escaped.get(o).filter(|_| holder) {
          push(k, format!("{} (class at {:?})", CLASS_HOW[how as usize], site(c)));
        } else if let Some(&c) = self.opaque_class.get(o) {
          push(k, format!("is a class instance (class at {:?})", site(c)));
        }
      }
    }
    for (o, (c, how)) in &self.class_escaped {
      if let ObjKind::Key { strings, .. } = self.objs[*o].kind {
        for st in &self.vars[strings].pts {
          if let ObjKind::Str(name) = &self.objs[*st].kind {
            if self.key_values.contains_key(name) {
              push(name, format!("its key {} (class at {:?})", CLASS_HOW[*how as usize], site(*c)));
            }
          }
        }
      }
    }
    out
  }

  pub fn bail_everything(&mut self, reason: &str) {
    self.bail_all = true;
    if self.bail_reasons.len() < 20 && !self.bail_reasons.iter().any(|r| r == reason) {
      self.bail_reasons.push(reason.to_string());
    }
  }

  /// A value handed to code the pass cannot see.
  pub fn escape(&mut self, obj: ObjId, flavour: Flavour, site: Option<Site>) {
    self.escape_with(obj, flavour, site, true);
  }

  /// `escape`. With `ret` false, a function that goes to effect's code goes without what it
  /// returns (`typed_callback`'s callbacks declared to return no service); the first
  /// escape that hands its return over, whenever it comes, does.
  fn escape_with(&mut self, obj: ObjId, flavour: Flavour, site: Option<Site>, ret: bool) {
    // An escape into effect's code runs once; one into code the pass can't see runs once too,
    // even after the first (it hands over more: values from unseen code may be anything).
    let into_effect = flavour == Flavour::Ext && site == Some(EFFECT_ESCAPE);
    // Only a function, or a record or array of them, has returns to hold back.
    let ret = ret
      || !into_effect
      || !matches!(self.objs[obj].kind, ObjKind::Closure(_) | ObjKind::Object | ObjKind::Array);
    let closure = matches!(self.objs[obj].kind, ObjKind::Closure(_));
    self.hand_key(obj, false);
    let repeat = if into_effect {
      self.objs[obj].escaped.is_some() || !self.effect_escaped.insert(obj)
    } else {
      self.objs[obj].escaped.is_some()
        && !(flavour == Flavour::Ext
          && self.effect_escaped.contains(&obj)
          && self.objs[obj].escaped == Some(Flavour::Ext)
          && self.genuine_escaped.insert(obj))
    };
    if repeat {
      // A later escape that hands over a return held back so far (whatever its order).
      if ret && self.ret_withheld.remove(&obj) {
        self.release_ret(obj);
      }
      return;
    }
    if !ret {
      self.ret_withheld.insert(obj);
    }
    if !into_effect && flavour == Flavour::Ext {
      self.genuine_escaped.insert(obj);
    }
    self.objs[obj].escaped = Some(flavour);
    self.recheck_tree(obj);
    self.touch_namespace(obj, None);
    // Host builtins never look inside their arguments (a wrong builtin model is a compiler
    // bug): a plain object handed to one keeps its shape, and runtime objects stay sealed. Only
    // the closures it holds become reachable, with unknown arguments. External code may do
    // anything with the ordinary values it receives, now or later: a service among them bails
    // its key. Effect's containers (Effect, Context, Layer, ManagedRuntime, MemoMap, Fiber),
    // keys and primitives are not a path to services, so they bail nothing, and neither do
    // the services reached only through them (issue, "Code the pass can't see").
    let external = flavour == Flavour::Ext;
    match self.objs[obj].kind.clone() {
      // A function is a holder too: its own properties go with it, then its body runs with
      // unknown arguments (`escape_fn`).
      ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } | ObjKind::Closure(_) => {
        if external {
          self.objs[obj].demand.all();
        }
        // A record's functions go without their returns when it does (`release_ret`).
        let held =
          if ret || closure { Constraint::Escape(flavour, site) } else { Constraint::EscapeHeld };
        let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
        for f in fields {
          self.constrain(f, held.clone());
        }
        let elem = self.elem(obj);
        self.constrain(elem, held);
        // Fields written from outside may hold anything (what effect's code made, for it). A
        // function handed only where it is declared a callback that returns no service has no
        // properties by that type, and a record of callbacks is read for them: effect's code
        // writes none (`release_ret` adds them).
        if external && ret {
          let top = if into_effect { self.top_effect } else { self.top_ext };
          self.unknown_store(obj, elem, top);
        }
        let sources = self.objs[obj].spread_from.clone();
        for src in sources {
          self.constrain(src, Constraint::Escape(flavour, site));
        }
        // What it inherits crosses with it (`box.inner` read through `Object.create`), including
        // a prototype installed after the handover.
        let proto = self.proto_var(obj);
        self.constrain(proto, Constraint::Escape(flavour, site));
        if let ObjKind::Class { prototype, ctor, heritage } = self.objs[obj].kind {
          self.constrain(prototype, Constraint::Escape(flavour, site));
          if let Some(f) = ctor {
            self.escape_fn(f, flavour, site);
          }
          if let Some(h) = heritage {
            self.constrain(h, Constraint::Escape(flavour, site));
          }
        }
        if let ObjKind::Closure(f) = self.objs[obj].kind {
          if ret {
            self.ret_withheld.remove(&obj);
          }
          self.escape_fn_with(f, flavour, site, ret);
        }
      }
      // A stand-in's values escape themselves (`apply` hands the escape to `held_var`).
      ObjKind::Typed { .. } if self.is_stand_in(obj) => {}
      ObjKind::Promise { value } | ObjKind::Wrap(value) | ObjKind::Typed { slot: value, .. } => {
        self.constrain(value, Constraint::Escape(flavour, site));
      }
      ObjKind::Iterator(f) => {
        self.activate(f);
        let ret = self.fns[f].ret;
        self.constrain(ret, Constraint::Escape(flavour, site));
      }
      ObjKind::Namespace => {
        // A module namespace hands over every export, as ordinary values.
        let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
        for f in fields {
          self.constrain(f, Constraint::Escape(flavour, site));
        }
        let elem = self.elem(obj);
        self.constrain(elem, Constraint::Escape(flavour, site));
      }
      // Effect's containers, keys and primitives bail nothing (see above).
      ObjKind::Context { .. }
      | ObjKind::Layer { .. }
      | ObjKind::Runtime { .. }
      | ObjKind::Effect { .. }
      | ObjKind::Key { .. }
      | ObjKind::Builtin { .. }
      | ObjKind::Fiber
      | ObjKind::MemoMap
      | ObjKind::Str(_)
      | ObjKind::Top(_)
      | ObjKind::External { .. }
      | ObjKind::AllKeys
      | ObjKind::Prim
      | ObjKind::Num(_) => {}
    }
  }

  /// Fields written from outside: `elem` may hold `top`.
  fn unknown_store(&mut self, obj: ObjId, elem: VarId, top: ObjId) {
    self.seed(elem, top);
    if !self.objs[obj].unknown_stores.contains(&elem) {
      self.objs[obj].unknown_stores.push(elem);
      self.graph_changed();
      self.recheck_tree(obj);
    }
  }

  /// The rest of an escape into effect's code held back for a function handed over so far only
  /// without its return (`escape_with`): its return goes there, and effect's code may write
  /// into it.
  fn release_ret(&mut self, obj: ObjId) {
    if matches!(self.objs[obj].kind, ObjKind::Object | ObjKind::Array) {
      let mut vars: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
      vars.push(self.elem(obj));
      for v in vars {
        self.constrain(v, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
      }
      return;
    }
    let ObjKind::Closure(f) = self.objs[obj].kind else { return };
    let elem = self.elem(obj);
    self.unknown_store(obj, elem, self.top_effect);
    let r = self.fns[f].ret;
    self.constrain(r, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
  }

  fn escape_fn(&mut self, f: FnId, flavour: Flavour, site: Option<Site>) {
    self.escape_fn_with(f, flavour, site, true);
  }

  fn escape_fn_with(&mut self, f: FnId, flavour: Flavour, site: Option<Site>, ret_escapes: bool) {
    self.activate(f);
    let (params, rest, this, ret) = {
      let i = &self.fns[f];
      (i.params.clone(), i.rest, i.this, i.ret)
    };
    let t = match flavour {
      Flavour::Ext if site == Some(EFFECT_ESCAPE) => self.top_effect,
      Flavour::Ext => self.top_ext,
      Flavour::Host => self.top_host,
    };
    for p in params {
      self.seed(p, t);
    }
    if let Some(r) = rest {
      let arr = self.rest_array(f);
      let elem = self.elem(arr);
      self.seed(elem, t);
      self.seed(r, arr);
    }
    self.seed(this, t);
    if ret_escapes {
      self.constrain(ret, Constraint::Escape(flavour, site));
    }
  }

  // == queries used after solving

  pub fn pts(&self, var: VarId) -> &FxHashSet<ObjId> {
    &self.vars[var].pts
  }

  pub fn key_is_bailed(&self, key: &str) -> bool {
    self.bail_all || self.never_prune.contains(key)
  }
}

impl Obj {
  pub fn new(kind: ObjKind, site: Option<Site>) -> Self {
    Self {
      kind,
      fields: FxHashMap::default(),
      elem: None,
      proto: None,
      spread_from: vec![],
      demand: Demand::None,
      escaped: None,
      unknown_loads: vec![],
      unknown_stores: vec![],
      spread_into: vec![],
      site,
      protocols: 0,
      builtin_proto: false,
      named_elem: None,
      named_loads: None,
    }
  }
}

pub fn model_kind_name(kind: &ObjKind) -> &'static str {
  match kind {
    ObjKind::Key { .. } => "Key",
    ObjKind::Context { .. } => "Context",
    ObjKind::Layer { .. } => "Layer",
    ObjKind::Effect { .. } => "Effect",
    ObjKind::Fiber => "Fiber",
    ObjKind::MemoMap => "MemoMap",
    ObjKind::Runtime { .. } => "Runtime",
    ObjKind::Promise { .. } => "Promise",
    ObjKind::Iterator(_) => "Iterator",
    ObjKind::Prim | ObjKind::Num(_) => "Prim",
    _ => "",
  }
}

/// The kinds whose named reads go on to the elements, spread sources and prototype
/// (`load_name`), and so record themselves (`Obj::named_loads`).
fn records_named_loads(kind: &ObjKind) -> bool {
  matches!(kind, ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. })
}

/// Whether a recorded named read of `name` on an object of `kind` reads its elements
/// (`load_name`): not one of effect's members, and on an array only an index.
fn reads_elem(kind: &ObjKind, name: &str) -> bool {
  !is_effect_member(name) && (!matches!(kind, ObjKind::Array) || is_index(name))
}

/// The rule of `Function.prototype.call`, `apply` or `bind`, for a read of `name` on a callable.
fn function_prototype_rule(a: &Analysis, name: &str) -> Option<u16> {
  a.rule_id(match name {
    "call" => "Function.prototype.call",
    "apply" => "Function.prototype.apply",
    "bind" => "Function.prototype.bind",
    _ => return None,
  })
}

/// A call site no source position has, for the protocol calls of host formatting.
fn protocol_site(n: u32) -> Site {
  Site { module: ModuleIdx::from_raw(0), start: u32::MAX - 16 - n, sub: 0 }
}

/// A canonical array index (`"0"`, `"12"`): a read by this name reads an element.
fn is_index(name: &str) -> bool {
  !name.is_empty()
    && name.bytes().all(|b| b.is_ascii_digit())
    && (name == "0" || !name.starts_with('0'))
}

/// One of effect's own members (`~effect/Context/Service` and the other type ids): written
/// only by code that names it, never by a store under a key computed from data. Effect's type
/// ids are internal names; program data and code the pass can't see do not produce them.
fn is_effect_member(name: &str) -> bool {
  name.starts_with("~effect/")
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A service provided under `test/Store`, and a set that widens past a limit of two values
  /// (three literal names). `runtime` is a value whose name is only known at run time; it
  /// joins the set before the literal names widen it, or after. Each step is solved before the
  /// next, so the lookup only arrives once the set is widened.
  fn widened_lookup(runtime: impl Fn(&mut Analysis) -> ObjId, early: bool, by_name: bool) -> bool {
    let mut a = Analysis::new();
    a.widen_limit = 2;
    let provided = a.alloc(ObjKind::Object, None);
    let store = a.key_value_var(&ArcStr::from("test/Store"));
    a.seed(store, provided);
    let set = a.var();
    let value = runtime(&mut a);
    if early {
      a.seed(set, value);
      a.solve();
    }
    for i in 0..3 {
      let name = a.str(&format!("test/Other{i}"));
      a.seed(set, name);
    }
    a.solve();
    assert!(a.vars[set].widened, "the set widens");
    if !early {
      a.seed(set, value);
      a.solve();
    }
    let dst = a.var();
    let use_ = KeyUse::Lookup(dst);
    a.constrain(set, if by_name { Constraint::KeyNameUse(use_) } else { Constraint::KeyUse(use_) });
    a.solve();
    a.pts(dst).contains(&provided)
  }

  /// A runtime name (`process.env.X`) may name any key, also once the set holding it widened.
  #[test]
  fn widened_runtime_name_names_any_key() {
    let host = |a: &mut Analysis| a.top_host;
    assert!(widened_lookup(host, true, true), "name before widening");
    assert!(widened_lookup(host, false, true), "name after widening");
  }

  /// A key-shaped object whose `key` is a runtime name, in a widened set: the opaque value it
  /// becomes may be that object, so a lookup through it sees every key.
  #[test]
  fn widened_key_shaped_runtime_name_names_any_key() {
    let keyed = |a: &mut Analysis| {
      let obj = a.alloc(ObjKind::Object, None);
      let key = a.field(obj, &ArcStr::from("key"));
      a.seed(key, a.top_host);
      obj
    };
    assert!(widened_lookup(keyed, true, false), "object before widening");
    assert!(widened_lookup(keyed, false, false), "object after widening");
  }

  /// An object crosses into program code the pass does not follow first, then joins a set that
  /// widens past a limit of two values. Its `key` is a runtime name, either from the start or
  /// written through the original alias after widening (the field is new then). Each step is
  /// solved before the next, so the widening finds the object already crossed.
  fn crossed_then_widened(late_write: bool) -> bool {
    let mut a = Analysis::new();
    a.widen_limit = 2;
    let provided = a.alloc(ObjKind::Object, None);
    let store = a.key_value_var(&ArcStr::from("test/Store"));
    a.seed(store, provided);
    let obj = a.alloc(ObjKind::Object, None);
    if !late_write {
      let key = a.field(obj, &ArcStr::from("key"));
      a.seed(key, a.top_host);
    }
    let alias = a.var();
    a.seed(alias, obj);
    let class = a.unfollowed_class();
    a.constrain(alias, Constraint::ClassEscape(class, CLASS_UNFOLLOWED));
    a.solve();
    let set = a.var();
    a.seed(set, obj);
    for i in 0..3 {
      let name = a.str(&format!("test/Other{i}"));
      a.seed(set, name);
    }
    a.solve();
    assert!(a.vars[set].widened, "the set widens");
    if late_write {
      let src = a.var();
      a.seed(src, a.top_host);
      a.constrain(alias, Constraint::Store { key: Key::Name(ArcStr::from("key")), src });
      a.solve();
    }
    let dst = a.var();
    a.constrain(set, Constraint::KeyUse(KeyUse::Lookup(dst)));
    a.solve();
    a.pts(dst).contains(&provided)
  }

  /// Widening reads a runtime name as any key's even in an object that crossed another class
  /// boundary first: the name it already holds.
  #[test]
  fn crossed_then_widened_held_runtime_name_names_any_key() {
    assert!(crossed_then_widened(false));
  }

  /// The same, for a runtime name written through the original alias after widening.
  #[test]
  fn crossed_then_widened_late_runtime_name_names_any_key() {
    assert!(crossed_then_widened(true));
  }

  /// A value crosses into program code the pass does not follow first (`first`), then joins
  /// a set that widens past a limit of two values, then `late` runs. Each step is solved
  /// before the next. Returns whether a lookup through the widened set (by key, or by name
  /// with `by_name`) sees the service provided under `test/Store`.
  fn crossed_then_widened_with(
    first: impl FnOnce(&mut Analysis) -> ObjId,
    late: impl FnOnce(&mut Analysis, ObjId),
    by_name: bool,
  ) -> bool {
    let mut a = Analysis::new();
    a.widen_limit = 2;
    let provided = a.alloc(ObjKind::Object, None);
    let store = a.key_value_var(&ArcStr::from("test/Store"));
    a.seed(store, provided);
    let obj = first(&mut a);
    let alias = a.var();
    a.seed(alias, obj);
    let class = a.unfollowed_class();
    a.constrain(alias, Constraint::ClassEscape(class, CLASS_UNFOLLOWED));
    a.solve();
    let set = a.var();
    a.seed(set, obj);
    for i in 0..3 {
      let name = a.str(&format!("test/Other{i}"));
      a.seed(set, name);
    }
    a.solve();
    assert!(a.vars[set].widened, "the set widens");
    late(&mut a, obj);
    a.solve();
    let dst = a.var();
    let use_ = KeyUse::Lookup(dst);
    a.constrain(set, if by_name { Constraint::KeyNameUse(use_) } else { Constraint::KeyUse(use_) });
    a.solve();
    a.pts(dst).contains(&provided)
  }

  /// `Object.assign(original, { key: process.env.X })` after widening: the source is a new
  /// spread edge of an object that crossed another boundary first.
  #[test]
  fn crossed_then_widened_late_spread_runtime_name_names_any_key() {
    let late = |a: &mut Analysis, target: ObjId| {
      let src = a.alloc(ObjKind::Object, None);
      let key = a.field(src, &ArcStr::from("key"));
      a.seed(key, a.top_host);
      let v = a.var();
      a.seed(v, src);
      a.constrain(v, Constraint::SpreadInto(target));
    };
    assert!(crossed_then_widened_with(|a| a.alloc(ObjKind::Object, None), late, false));
  }

  /// A function that crossed another boundary first, then widened: what it returns (a runtime
  /// name) is what a call of the widened value may return, so a lookup by it sees every key.
  /// `late` gives the function's return the runtime name only after widening.
  fn crossed_then_widened_fn(late_return: bool) -> bool {
    let first = |a: &mut Analysis| {
      let vars = [a.var(), a.var(), a.var(), a.var(), a.var()];
      let f = a.fns.push(FnInfo::new(ModuleIdx::from_raw_unchecked(0), 0, FnKind::Arrow, vars));
      let closure = a.alloc(ObjKind::Closure(f), None);
      a.fns[f].closure = Some(closure);
      if !late_return {
        let ret = a.fns[f].ret;
        a.seed(ret, a.top_host);
      }
      closure
    };
    let late = move |a: &mut Analysis, closure: ObjId| {
      if late_return {
        let ObjKind::Closure(f) = a.objs[closure].kind else { unreachable!() };
        let ret = a.fns[f].ret;
        a.seed(ret, a.top_host);
      }
    };
    crossed_then_widened_with(first, late, true)
  }

  #[test]
  fn crossed_then_widened_fn_return_names_any_key() {
    assert!(crossed_then_widened_fn(false));
  }

  #[test]
  fn crossed_then_widened_fn_late_return_names_any_key() {
    assert!(crossed_then_widened_fn(true));
  }

  /// A function that returns an object is handed to effect's code twice: once without its
  /// return (a callback declared to return no service) and once by `second`. Each step is
  /// solved before the next, in the order given. Returns whether the object it returns went to
  /// effect's code.
  fn withheld_return(withheld_first: bool, second: Option<fn(&mut Analysis, ObjId)>) -> bool {
    let mut a = Analysis::new();
    let vars = [a.var(), a.var(), a.var(), a.var(), a.var()];
    let f = a.fns.push(FnInfo::new(ModuleIdx::from_raw_unchecked(0), 0, FnKind::Arrow, vars));
    let closure = a.alloc(ObjKind::Closure(f), None);
    a.fns[f].closure = Some(closure);
    let returned = a.alloc(ObjKind::Object, None);
    let ret = a.fns[f].ret;
    a.seed(ret, returned);
    let withhold = |a: &mut Analysis| {
      a.escape_with(closure, Flavour::Ext, Some(EFFECT_ESCAPE), false);
      a.solve();
    };
    if withheld_first {
      withhold(&mut a);
    }
    if let Some(second) = second {
      second(&mut a, closure);
      a.solve();
    }
    if !withheld_first {
      withhold(&mut a);
    }
    a.objs[returned].escaped.is_some()
  }

  /// Withholding a callback's return depends on no order: a later or earlier handover that
  /// passes the return (into effect's code, unseen code or a host builtin) sends it, and only a
  /// function never handed over with its return keeps it.
  #[test]
  fn withheld_return_is_order_independent() {
    let effect: fn(&mut Analysis, ObjId) = |a, f| a.escape(f, Flavour::Ext, Some(EFFECT_ESCAPE));
    let unseen: fn(&mut Analysis, ObjId) = |a, f| a.escape(f, Flavour::Ext, None);
    let host: fn(&mut Analysis, ObjId) = |a, f| a.escape(f, Flavour::Host, None);
    let constrained: fn(&mut Analysis, ObjId) = |a, f| {
      let v = a.var();
      a.seed(v, f);
      a.constrain(v, Constraint::Escape(Flavour::Ext, Some(EFFECT_ESCAPE)));
    };
    for (name, second) in
      [("effect", effect), ("unseen", unseen), ("host", host), ("var", constrained)]
    {
      assert!(withheld_return(true, Some(second)), "{name} after the withheld handover");
      assert!(withheld_return(false, Some(second)), "{name} before the withheld handover");
    }
    assert!(!withheld_return(true, None), "only withheld: the return stays");
  }

  /// The kinds of value whose admission waits for the solve to settle (`admit_typed_with`).
  #[derive(Clone, Copy, Debug)]
  enum Held {
    Effect,
    Promise,
    /// A type-modelled result (`Stream.make(record)`): what it holds is its slot.
    Typed,
  }

  const HELD: [Held; 3] = [Held::Effect, Held::Promise, Held::Typed];

  /// A fresh value of kind `held`, with the variable what it succeeds with or holds arrives in.
  fn held_value(a: &mut Analysis, held: Held, start: u32) -> (ObjId, VarId) {
    match held {
      Held::Effect => {
        let (o, success, _) =
          a.new_effect(Site { module: ModuleIdx::from_raw_unchecked(0), start, sub: 0 }, 0);
        (o, success)
      }
      Held::Promise => {
        let value = a.var();
        (a.alloc(ObjKind::Promise { value }, None), value)
      }
      Held::Typed => {
        let slot = a.var();
        (a.alloc(ObjKind::Typed { slot, export: NO_EXPORT }, None), slot)
      }
    }
  }

  /// A value is handed to type-modelled code (`AdmitTyped`) while what it succeeds with or
  /// holds arrives through a chain of copies, set up after or before the handover
  /// (`admission_first`) and solved once. `service`: what arrives is a provided service rather
  /// than plain data. Returns whether the value was admitted and whether what arrived went to
  /// effect's code.
  fn admitted_late(held: Held, admission_first: bool, service: bool) -> (bool, bool) {
    let mut a = Analysis::new();
    let (handed_value, success) = held_value(&mut a, held, 1);
    let value = a.alloc(ObjKind::Object, None);
    if service {
      a.seed(a.all_values, value);
      a.seed(a.unknown_key_values, value);
    }
    let (handed, dst) = (a.var(), a.var());
    let chain = [a.var(), a.var(), a.var()];
    let admit = |a: &mut Analysis| {
      a.seed(handed, handed_value);
      a.constrain(handed, Constraint::AdmitTyped(dst, false));
    };
    // The copies exist before the value does, so it travels through them during the solve.
    let arrive = |a: &mut Analysis| {
      a.copy(chain[0], chain[1]);
      a.copy(chain[1], chain[2]);
      a.copy(chain[2], success);
      a.seed(chain[0], value);
    };
    if admission_first {
      admit(&mut a);
      arrive(&mut a);
    } else {
      arrive(&mut a);
      admit(&mut a);
    }
    a.solve();
    (a.vars[dst].pts.contains(&handed_value), a.objs[value].escaped.is_some())
  }

  /// Admission is decided at the fixpoint, not when the handover is met: a value that comes to
  /// succeed with or hold a service is admitted, and the service stays the program's, in either
  /// order. One that never does goes to effect's code with what it holds.
  #[test]
  fn admission_is_order_independent() {
    for held in HELD {
      for first in [true, false] {
        assert_eq!(admitted_late(held, first, true), (true, false), "{held:?} service, {first}");
        assert_eq!(admitted_late(held, first, false), (false, true), "{held:?} data, {first}");
      }
    }
  }

  /// The shape of a `dependent_admissions` case.
  #[derive(Clone, Copy, Debug)]
  struct Dependent {
    held: Held,
    through_copy: bool,
    first_met: bool,
  }

  /// Two handovers wait for the solve to settle: `first` succeeds with what arrives through
  /// copies, and what `first`'s admission hands on is what `second` succeeds with, directly
  /// (`through_copy` false: `first`'s destination is `second`'s success) or through a copy.
  /// Both are of kind `held`, and the handovers are met in the order given (`first_met`).
  /// `service`: what arrives is a provided service rather than plain data. Returns whether
  /// each was admitted and whether what arrived went to effect's code.
  fn dependent_admissions(case: Dependent, service: bool) -> (bool, bool, bool) {
    let Dependent { held, through_copy, first_met } = case;
    let mut a = Analysis::new();
    let (first, first_success) = held_value(&mut a, held, 1);
    let (second, second_success) = held_value(&mut a, held, 2);
    let first_dst = if through_copy { a.var() } else { second_success };
    let second_dst = a.var();
    if first_met {
      a.admit_typed_with(first, first_dst, true);
      a.admit_typed_with(second, second_dst, true);
    } else {
      a.admit_typed_with(second, second_dst, true);
      a.admit_typed_with(first, first_dst, true);
    }
    assert_eq!(a.held_admissions.len(), 2, "both wait for the solve to settle");
    if through_copy {
      a.copy(first_dst, second_success);
    }
    let value = a.alloc(ObjKind::Object, None);
    if service {
      a.seed(a.all_values, value);
      a.seed(a.unknown_key_values, value);
    }
    let chain = [a.var(), a.var()];
    a.copy(chain[0], chain[1]);
    a.copy(chain[1], first_success);
    a.seed(chain[0], value);
    a.solve();
    (
      a.vars[first_dst].pts.contains(&first),
      a.vars[second_dst].pts.contains(&second),
      a.objs[value].escaped.is_some(),
    )
  }

  /// An admission that makes another value admissible is propagated before any value still
  /// held back goes to effect's code: both are admitted and the service stays the program's,
  /// whichever handover the solve met first, and whether the second sees the first directly
  /// or only once a copy has run. When nothing arrives that may be a service, both go, and so
  /// does what arrived.
  #[test]
  fn dependent_admissions_are_order_independent() {
    let mut wrong = vec![];
    for held in HELD {
      for through_copy in [false, true] {
        for first_met in [true, false] {
          for (service, expected) in [(true, (true, true, false)), (false, (false, false, true))] {
            let case = Dependent { held, through_copy, first_met };
            let got = dependent_admissions(case, service);
            if got != expected {
              wrong.push(format!("{case:?}, service {service}: {got:?}"));
            }
          }
        }
      }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
  }

  /// A value given up at one fixpoint keeps its subscription: a service it comes to succeed
  /// with or hold in a later phase of the solve goes to effect's code (bails), and the value is
  /// not admitted after the fact.
  #[test]
  fn released_admission_keeps_its_subscription() {
    for held in HELD {
      let mut a = Analysis::new();
      let (handed, success) = held_value(&mut a, held, 1);
      let dst = a.var();
      a.admit_typed_with(handed, dst, true);
      a.solve();
      assert!(a.held_admissions.is_empty(), "{held:?}: released at the fixpoint");
      let service = a.alloc(ObjKind::Object, None);
      a.seed(a.all_values, service);
      a.seed(a.unknown_key_values, service);
      a.seed(success, service);
      a.solve();
      assert!(!a.vars[dst].pts.contains(&handed), "{held:?}: not admitted later");
      assert!(a.objs[service].escaped.is_some(), "{held:?}: the late service bails");
    }
  }

  /// The edge a `late_edge_turns_negative_positive` case adds once the negatives are cached.
  #[derive(Clone, Copy, Debug)]
  enum LateEdge {
    Spread,
    UnknownStore,
    Field,
  }

  /// `reaches` shares negative results between walks while the graph is unchanged
  /// (`Marks`). An edge to a source that already holds a service, added after `target` and
  /// `outer` (whose field holds `target`) were found to hold none, must turn both positive:
  /// the edge changes the graph, so the cached negatives are not reused.
  fn late_edge_turns_negative_positive(edge: LateEdge) -> (bool, bool) {
    let mut a = Analysis::new();
    let service = a.alloc(ObjKind::Object, None);
    a.seed(a.all_values, service);
    a.seed(a.unknown_key_values, service);
    let source = a.alloc(ObjKind::Object, None);
    let held = a.field(source, &ArcStr::from("service"));
    a.seed(held, service);
    // The variable a spread reads the source through, populated before the negatives.
    let source_var = a.holder(source);
    let target = a.alloc(ObjKind::Object, None);
    let outer = a.alloc(ObjKind::Object, None);
    let slot = a.field(outer, &ArcStr::from("target"));
    a.seed(slot, target);
    assert!(!a.may_hold(outer), "{edge:?}: outer holds no service yet");
    assert!(!a.may_hold(target), "{edge:?}: target holds no service yet");
    match edge {
      LateEdge::Spread => a.spread_into(source, target),
      // The store goes through an admission filter, which the solve fills.
      LateEdge::UnknownStore => {
        a.store_unknown(target, source_var);
        a.solve();
      }
      LateEdge::Field => {
        let late = a.field(target, &ArcStr::from("late"));
        a.seed(late, source);
      }
    }
    (a.may_hold(target), a.may_hold(outer))
  }

  #[test]
  fn late_edge_invalidates_cached_negatives() {
    for edge in [LateEdge::Spread, LateEdge::UnknownStore, LateEdge::Field] {
      assert_eq!(late_edge_turns_negative_positive(edge), (true, true), "{edge:?}");
    }
  }

  /// A chain of `n` objects, each holding the next in a field: a walk from its head visits
  /// `n`. (One set holding many objects would widen.)
  fn chain(a: &mut Analysis, n: usize) -> ObjId {
    let next = ArcStr::from("next");
    let head = a.alloc(ObjKind::Object, None);
    let mut last = head;
    for _ in 1..n {
      let obj = a.alloc(ObjKind::Object, None);
      let f = a.field(last, &next);
      a.seed(f, obj);
      last = obj;
    }
    head
  }

  /// The visit limit (`reaches`'s `LIMIT`, 1 << 17 objects) holds per walk. Negative walks
  /// over an unchanged graph share their marks, so their visits add up: three chains of 45,000
  /// objects each cross the limit together but not alone, and each still holds no service.
  /// A root whose own walk crosses the limit counts as holding one (the conservative answer),
  /// whether it is walked before or after the others.
  #[test]
  fn visit_limit_is_per_root() {
    for big_first in [false, true] {
      let mut a = Analysis::new();
      let small: Vec<ObjId> = (0..3).map(|_| chain(&mut a, 45_000)).collect();
      let big = chain(&mut a, 132_000);
      let mut got = vec![];
      if big_first {
        assert!(a.may_hold(big), "over the limit alone, walked first");
      }
      for root in &small {
        got.push(a.may_hold(*root));
      }
      if !big_first {
        assert!(a.may_hold(big), "over the limit alone, walked last");
      }
      assert_eq!(got, vec![false; 3], "big first: {big_first}");
    }
  }

  // == shared receiver members (`receiver_member`)
  //
  // Each case builds the same analysis twice, with a fresh member per call site (the
  // construction before sharing, `fresh_receiver_members`) and with shared members, under the
  // same events. It compares what every site's check did, by name rather than by variable:
  // what each site's member held, whether its check handed the receiver over, what reached its
  // slot, which values escaped or crossed the widening boundary, what was demanded of them, and
  // whether the budget stopped the solve.

  /// The objects and variables a receiver case reads its outcome from. Its limits: an object it
  /// has no name for is labelled by its kind, so two unnamed objects of one kind (two opaque
  /// values, say) look the same; and the sites' `back` variables are not read, so per-site back
  /// flow is not compared. The cases name every object whose identity they assert on.
  #[derive(Default)]
  struct Probe {
    objs: Vec<(String, ObjId)>,
    vars: Vec<(String, VarId)>,
    slots: Vec<VarId>,
  }

  impl Probe {
    fn label(&self, a: &Analysis, o: ObjId) -> String {
      match self.objs.iter().find(|(_, x)| *x == o) {
        Some((name, _)) => name.clone(),
        None => {
          format!("{:?}", a.objs[o].kind).split([' ', '(', '{']).next().unwrap_or("").to_string()
        }
      }
    }

    fn set(&self, a: &Analysis, v: VarId) -> Vec<String> {
      let mut labels: Vec<String> = a.vars[v].pts.iter().map(|o| self.label(a, *o)).collect();
      labels.sort();
      labels
    }

    fn outcome(&self, a: &Analysis) -> Vec<String> {
      let mut out = vec![format!("budget stop {}, bail {}", a.budget_exceeded, a.bail_all)];
      for (i, slot) in self.slots.iter().enumerate() {
        // What a site hands a wrapped value's contents to (`typed_arg`).
        let slot_in: Vec<&str> = self
          .vars
          .iter()
          .filter(|(_, v)| {
            a.vars[*v]
              .constraints
              .iter()
              .any(|c| matches!(c, Constraint::SlotIn(s, _) if s == slot))
          })
          .map(|(n, _)| n.as_str())
          .collect();
        // The site's member, and whether its check fired: handed the receiver to `typed_arg`.
        let check = a.vars.iter_enumerated().find_map(|(v, var)| {
          var.constraints.iter().find_map(|c| match c {
            Constraint::TypedReceiverIf(_, recv, s, ..) if s == slot => Some((v, *recv)),
            _ => None,
          })
        });
        let (member, handed) = match check {
          Some((v, recv)) => (
            format!("{:?} (widened {})", self.set(a, v), a.vars[v].widened),
            a.typed_args_seen.contains(&(recv, *slot, false, false)),
          ),
          None => ("none".to_string(), false),
        };
        out.push(format!(
          "site {i}: member {member}, handed over {handed}, slot {:?}, widened {}, slot-in from {slot_in:?}",
          self.set(a, *slot),
          a.vars[*slot].widened
        ));
      }
      for (name, v) in &self.vars {
        out.push(format!("{name}: {:?}, widened {}", self.set(a, *v), a.vars[*v].widened));
      }
      for (name, o) in &self.objs {
        let obj = &a.objs[*o];
        let demand = match &obj.demand {
          Demand::None => String::new(),
          Demand::All => "all".to_string(),
          Demand::Names(names) => {
            let mut names: Vec<&str> = names.iter().map(ArcStr::as_str).collect();
            names.sort_unstable();
            names.join(",")
          }
        };
        out.push(format!(
          "{name}: escaped {:?}, class {:?}, widen-escaped {}, demand [{demand}]",
          obj.escaped,
          a.class_escaped.get(o).map(|(_, how)| *how),
          a.widen_escaped.contains(o)
        ));
      }
      out
    }
  }

  /// The variables that hold a `TypedReceiverIf` check: one per site, or per shared member.
  fn members(a: &Analysis) -> usize {
    a.vars
      .iter()
      .filter(|v| v.constraints.iter().any(|c| matches!(c, Constraint::TypedReceiverIf(..))))
      .count()
  }

  fn site(i: usize) -> Site {
    Site { module: ModuleIdx::from_raw_unchecked(0), start: 1 + u32::try_from(i).unwrap(), sub: 0 }
  }

  /// A callee summary: what `o.name(...)` calls in type-modelled `effect` code.
  fn summary(a: &mut Analysis) -> ObjId {
    let slot = a.var();
    a.alloc(ObjKind::Typed { slot, export: NO_EXPORT }, None)
  }

  #[derive(Clone, Copy, Debug)]
  enum Event {
    /// Registers a site (`TypedReceiver`).
    Site(usize),
    /// The callee summary of sites 0, 2 and 3 arrives.
    Callee,
    /// Another value arrives (a filler, toward the widening limit).
    Fill,
    /// Excluded-kind cases: the receiver gets a prototype whose `map` holds the callee.
    Proto,
    /// The wrapped values widen. The next solve propagates it, so a site registered after this
    /// event is applied after the propagation; `staged_pending_widening` covers a site applied
    /// before it.
    WidenWrapped,
  }

  /// A receiver case: the receiver kind and the sites on it, each `(callee, name)`. Callee 0
  /// arrives with `Event::Callee`, callee 1 never does.
  #[derive(Clone, Copy, Debug)]
  enum Receiver {
    Wrap,
    /// A string with no registered methods: every name reads host data.
    Str,
    /// A string whose `map` and `slice` are registered builtins, which are the callees
    /// (callee 0 is `String.prototype.map`, callee 1 `String.prototype.slice`): a name's
    /// member holds that name's builtin and no other.
    StrMethods,
    Object,
    Namespace,
  }

  const SITES: [(usize, &str); 5] =
    [(0, "map"), (1, "map"), (0, "map"), (0, "slice"), (1, "slice")];

  /// The analysis of a receiver case before any event.
  struct Case {
    a: Analysis,
    p: Probe,
    callees: [ObjId; 2],
    fillers: Vec<ObjId>,
    recv: VarId,
    recv_obj: ObjId,
    arrivals: Option<VarId>,
    backs: Vec<VarId>,
  }

  impl Case {
    /// Site `i`'s `TypedReceiver` constraint.
    fn site(&self, i: usize) -> Constraint {
      let (callee, name) = SITES[i];
      Constraint::TypedReceiver(
        self.callees[callee],
        ArcStr::from(name),
        self.p.slots[i],
        self.backs[i],
        site(i),
      )
    }
  }

  fn receiver_setup(kind: Receiver, fresh: bool, limit: usize, budget: Option<u64>) -> Case {
    let mut a = Analysis::new();
    a.fresh_receiver_members = fresh;
    a.widen_limit = limit;
    if let Some(budget) = budget {
      a.max_entries = budget;
    }
    let mut p = Probe::default();
    let callees = if let Receiver::StrMethods = kind {
      let names = ["String.prototype.map", "String.prototype.slice"];
      names.map(|name| {
        let rule = a.rule(name, |_, _| {});
        let b = a.builtin(rule, None, None);
        p.objs.push((name.into(), b));
        b
      })
    } else {
      let callees = [summary(&mut a), summary(&mut a)];
      p.objs.push(("callee0".into(), callees[0]));
      p.objs.push(("callee1".into(), callees[1]));
      callees
    };
    let service = a.alloc(ObjKind::Object, None);
    p.objs.push(("service".into(), service));
    let fillers: Vec<ObjId> = (0..3).map(|_| a.alloc(ObjKind::Object, None)).collect();
    for (i, f) in fillers.iter().enumerate() {
      p.objs.push((format!("filler{i}"), *f));
    }
    // Where callee 0 and the fillers arrive: the wrapped values, or the receiver's own `map`.
    let (recv_obj, arrivals) = match kind {
      Receiver::Wrap => {
        let inner = a.var();
        a.seed(inner, service);
        p.vars.push(("wrapped".into(), inner));
        (a.alloc(ObjKind::Wrap(inner), None), Some(inner))
      }
      Receiver::Str | Receiver::StrMethods => (a.str("receiver"), None),
      Receiver::Object => {
        let obj = a.alloc(ObjKind::Object, None);
        let own = a.field(obj, &ArcStr::from("map"));
        p.vars.push(("own map".into(), own));
        (obj, Some(own))
      }
      Receiver::Namespace => {
        let ns = a.alloc(ObjKind::Namespace, None);
        // A lazy export: its body runs once `map` is read, and seeds `ran` with `service`.
        let [this, ret, yielded_fail, yielded, args] = std::array::from_fn(|_| a.var());
        let mut f = FnInfo::new(
          ModuleIdx::from_raw_unchecked(0),
          0,
          FnKind::Normal,
          [this, ret, yielded_fail, yielded, args],
        );
        let ran = a.var();
        f.pending.push(Pending::Seed(ran, service));
        let f = a.fns.push(f);
        a.lazy_exports.insert((ns, ArcStr::from("map")), f);
        p.vars.push(("lazy export ran".into(), ran));
        (ns, None)
      }
    };
    p.objs.push(("receiver".into(), recv_obj));
    let recv = a.var();
    a.seed(recv, recv_obj);
    p.slots = SITES.iter().map(|_| a.var()).collect();
    let backs: Vec<VarId> = SITES.iter().map(|_| a.var()).collect();
    Case { a, p, callees, fillers, recv, recv_obj, arrivals, backs }
  }

  /// Runs `events` (solving after each one, or once at the end) on a receiver of `kind`, with
  /// the widening limit and entries budget given. Returns the outcome and the member count.
  fn receiver_case(
    kind: Receiver,
    fresh: bool,
    events: &[Event],
    solve_each: bool,
    limit: usize,
    budget: Option<u64>,
  ) -> (Vec<String>, usize) {
    let case = receiver_setup(kind, fresh, limit, budget);
    let c = &case;
    let sites: Vec<Constraint> = (0..SITES.len()).map(|i| c.site(i)).collect();
    let Case { mut a, p, callees, fillers, recv, arrivals, .. } = case;
    let mut filled = 0;
    for event in events {
      match *event {
        Event::Site(i) => a.constrain(recv, sites[i].clone()),
        Event::Callee => a.seed(arrivals.expect("a place to arrive"), callees[0]),
        Event::Fill => {
          a.seed(arrivals.expect("a place to arrive"), fillers[filled]);
          filled += 1;
        }
        Event::WidenWrapped => {
          let wrapped = arrivals.expect("wrapped values");
          if !a.vars[wrapped].widened {
            a.widen(wrapped);
          }
        }
        Event::Proto => {
          let proto = a.alloc(ObjKind::Object, None);
          let map = a.field(proto, &ArcStr::from("map"));
          a.seed(map, callees[0]);
          let src = a.var();
          a.seed(src, proto);
          a.constrain(recv, Constraint::SetProto(src));
        }
      }
      if solve_each {
        a.solve();
      }
    }
    a.solve();
    (p.outcome(&a), members(&a))
  }

  /// Every order of `events` (distinct orders only).
  fn orders(events: &[Event]) -> Vec<Vec<Event>> {
    let mut seen: FxHashSet<String> = FxHashSet::default();
    let mut out = vec![];
    let mut idx: Vec<usize> = (0..events.len()).collect();
    permute(&mut idx, 0, &mut |order| {
      let events: Vec<Event> = order.iter().map(|i| events[*i]).collect();
      if seen.insert(format!("{events:?}")) {
        out.push(events);
      }
    });
    out
  }

  fn permute(idx: &mut Vec<usize>, k: usize, f: &mut impl FnMut(&[usize])) {
    if k == idx.len() {
      f(idx);
      return;
    }
    for i in k..idx.len() {
      idx.swap(k, i);
      permute(idx, k + 1, f);
      idx.swap(k, i);
    }
  }

  /// Compares the two constructions over every order of `events`, solving after each event or
  /// once, under each widening limit. Returns the mismatches and the member counts (fresh,
  /// shared) summed over the runs.
  fn compare(kind: Receiver, events: &[Event], limits: &[usize]) -> (Vec<String>, (usize, usize)) {
    let mut wrong = vec![];
    let mut counts = (0, 0);
    for order in orders(events) {
      for solve_each in [true, false] {
        for &limit in limits {
          let (old, old_members) = receiver_case(kind, true, &order, solve_each, limit, None);
          let (new, new_members) = receiver_case(kind, false, &order, solve_each, limit, None);
          counts.0 += old_members;
          counts.1 += new_members;
          if old != new && wrong.len() < 4 {
            let diff: Vec<String> = old
              .iter()
              .zip(&new)
              .filter(|(o, n)| o != n)
              .map(|(o, n)| format!("fresh {o} / shared {n}"))
              .collect();
            wrong
              .push(format!("{kind:?} {order:?} solve each {solve_each} limit {limit}: {diff:#?}"));
          }
        }
      }
    }
    (wrong, counts)
  }

  const ALL_SITES: [Event; 4] = [Event::Site(0), Event::Site(1), Event::Site(2), Event::Site(3)];

  /// Guard 1, late arrival and site isolation. Sites on one wrapped value, registered in every
  /// order relative to each other and to the callee's arrival: each site's check acts for that
  /// site alone. A site whose callee never arrives hands nothing over, two sites with the same
  /// callee each hand the receiver to their own slot, and the shared construction does exactly
  /// what fresh members did.
  #[test]
  fn shared_receiver_member_keeps_sites_apart() {
    let mut events = ALL_SITES.to_vec();
    events.push(Event::Callee);
    let (wrong, (fresh, shared)) = compare(Receiver::Wrap, &events, &[400]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(shared < fresh, "the members are shared: {shared} of {fresh}");
    let (outcome, _) = receiver_case(Receiver::Wrap, false, &events, true, 400, None);
    assert_eq!(
      outcome[1..=4],
      [
        "site 0: member [\"callee0\", \"receiver\", \"service\"] (widened false), handed over \
         true, slot [\"receiver\"], widened false, slot-in from [\"wrapped\"]",
        "site 1: member [\"callee0\", \"receiver\", \"service\"] (widened false), handed over \
         false, slot [], widened false, slot-in from []",
        "site 2: member [\"callee0\", \"receiver\", \"service\"] (widened false), handed over \
         true, slot [\"receiver\"], widened false, slot-in from [\"wrapped\"]",
        "site 3: member [\"callee0\", \"receiver\", \"service\"] (widened false), handed over \
         true, slot [\"receiver\"], widened false, slot-in from [\"wrapped\"]",
      ],
    );
    // A string with no registered methods: every name's member holds host data. Names with
    // distinct builtins are `shared_string_member_keeps_names_apart`.
    let (wrong, (fresh, shared)) = compare(Receiver::Str, &ALL_SITES, &[400]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(shared < fresh, "the members are shared: {shared} of {fresh}");
  }

  /// Guard 1, excluded kinds. An object whose prototype arrives between sites, and a namespace
  /// with a lazy export: the memo is not consulted (a member per site), and the outcome is the
  /// fresh construction's.
  #[test]
  fn excluded_receivers_keep_fresh_members() {
    let mut events = vec![Event::Site(0), Event::Site(1), Event::Site(2)];
    events.extend([Event::Proto, Event::Fill]);
    let (wrong, (fresh, shared)) = compare(Receiver::Object, &events, &[400]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert_eq!(fresh, shared, "a member per site");
    let (wrong, (fresh, shared)) = compare(Receiver::Namespace, &ALL_SITES, &[400]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert_eq!(fresh, shared, "a member per site");
  }

  /// Guard 2, lookup effects. Reading `map` and `slice` off a wrapped value demands those
  /// names of the values it wraps, whichever site reads first; an unread name is not demanded.
  /// A namespace's lazy export runs on the first read, as before.
  #[test]
  fn shared_receiver_member_keeps_lookup_effects() {
    for (events, demand) in [
      (vec![Event::Site(0), Event::Site(2)], "[map]"),
      (vec![Event::Site(2), Event::Site(3), Event::Site(0)], "[map,slice]"),
    ] {
      let (old, _) = receiver_case(Receiver::Wrap, true, &events, true, 400, None);
      let (new, _) = receiver_case(Receiver::Wrap, false, &events, true, 400, None);
      assert_eq!(old, new, "{events:?}");
      let service = new.iter().find(|l| l.starts_with("service:")).unwrap();
      assert!(service.ends_with(&format!("demand {demand}")), "{events:?}: {service}");
    }
    let (new, _) = receiver_case(Receiver::Namespace, false, &[Event::Site(1)], true, 400, None);
    assert!(new.contains(&"lazy export ran: [\"service\"], widened false".to_string()), "{new:#?}");
  }

  /// Guard 3, the widening boundary. The callee and fillers arrive in every order around the
  /// sites, with limits that widen the shared member, or the wrapped values themselves, before,
  /// between and after the sites; and the wrapped values widen between sites (propagated before
  /// the next site is applied). Each site's check sees what it saw with a fresh member. After
  /// the member widens, the next site gets a fresh member that later sites share again; after
  /// the wrapped values widen, every later site gets its own.
  #[test]
  fn shared_receiver_member_widens_as_fresh_members() {
    let events =
      [Event::Site(0), Event::Site(1), Event::Site(2), Event::Callee, Event::Fill, Event::Fill];
    let (wrong, (fresh, shared)) = compare(Receiver::Wrap, &events, &[1, 2, 3, 4]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(shared < fresh, "the members are shared: {shared} of {fresh}");
    let events =
      [Event::Site(0), Event::Site(1), Event::Site(2), Event::Callee, Event::WidenWrapped];
    let (wrong, (fresh, shared)) = compare(Receiver::Wrap, &events, &[400]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(shared < fresh, "the members are shared: {shared} of {fresh}");
  }

  /// Guard 3, the budget. A budget the analysis exceeds before any site stops both
  /// constructions the same way: every method kept.
  #[test]
  fn shared_receiver_member_budget_stop() {
    let mut events = ALL_SITES.to_vec();
    events.push(Event::Callee);
    let (old, _) = receiver_case(Receiver::Wrap, true, &events, true, 400, Some(1));
    let (new, _) = receiver_case(Receiver::Wrap, false, &events, true, 400, Some(1));
    assert_eq!(old, new);
    assert_eq!(new[0], "budget stop true, bail true");
  }

  /// Guard 1, names. One string whose `map` and `slice` are distinct registered builtins, and
  /// sites whose callee is the builtin of their own name (0, 2, 4) or of the other name (1, 3),
  /// with `map` read twice. Each name's member holds its own builtin only, so a site's check
  /// fires exactly when its callee is the builtin of the name it reads, whichever name is read
  /// first. A member shared across names would hold both builtins and fire the cross-name sites.
  #[test]
  fn shared_string_member_keeps_names_apart() {
    let map_first =
      [Event::Site(0), Event::Site(4), Event::Site(1), Event::Site(3), Event::Site(2)];
    let slice_first =
      [Event::Site(4), Event::Site(3), Event::Site(0), Event::Site(2), Event::Site(1)];
    for events in [map_first, slice_first] {
      for solve_each in [true, false] {
        let (old, _) = receiver_case(Receiver::StrMethods, true, &events, solve_each, 400, None);
        let (new, members) =
          receiver_case(Receiver::StrMethods, false, &events, solve_each, 400, None);
        assert_eq!(old, new, "{events:?} solve each {solve_each}");
        assert_eq!(members, 2, "a member per name");
        let sites: Vec<String> =
          new[1..=5].iter().map(|l| l.split(", slot ").next().unwrap().to_string()).collect();
        assert_eq!(
          sites,
          [
            "site 0: member [\"String.prototype.map\"] (widened false), handed over true",
            "site 1: member [\"String.prototype.map\"] (widened false), handed over false",
            "site 2: member [\"String.prototype.map\"] (widened false), handed over true",
            "site 3: member [\"String.prototype.slice\"] (widened false), handed over false",
            "site 4: member [\"String.prototype.slice\"] (widened false), handed over true",
          ],
          "{events:?} solve each {solve_each}"
        );
      }
    }
  }

  /// What `staged_pending_widening` saw: the outcome, and whether the staging held.
  struct Staged {
    outcome: Vec<String>,
    /// Shared construction: site 0's member, cached under (receiver, `map`) before the widening.
    cached: Option<VarId>,
    /// The widening was still queued when site 2 was applied, and after.
    pending: bool,
    /// The cached member did not yet hold the widened stand-in when site 2 was applied.
    behind: bool,
    /// Members after site 1 was applied too, and the memo entry for (receiver, `map`) then.
    members: usize,
    memo_after: Option<VarId>,
  }

  /// Site 0 on a wrapped value holding the service and callee 0 is applied and solved, so its
  /// member holds both. The wrapped values then widen, and site 2 (callee 0, `map`) is applied
  /// as the solve applies a new constraint, to the receiver's objects before the worklist runs,
  /// so before the widening has reached site 0's member. Then the solve drains, and site 1
  /// (callee 1, `map`) is registered and solved after the widening has propagated.
  fn staged_pending_widening(fresh: bool) -> Staged {
    let mut c = receiver_setup(Receiver::Wrap, fresh, 400, None);
    let inner = c.arrivals.expect("wrapped values");
    c.a.seed(inner, c.callees[0]);
    let site0 = c.site(0);
    c.a.constrain(c.recv, site0);
    c.a.solve();
    let cached = c.a.receiver_members.get(&(c.recv_obj, ArcStr::from("map"))).copied();
    c.a.widen(inner);
    let stand_in = c.a.vars[inner].delta[0];
    c.p.objs.push(("widened stand-in".into(), stand_in));
    let queued = c.a.vars[inner].queued && c.a.vars[inner].delta == [stand_in];
    let behind = cached.is_some_and(|m| !c.a.vars[m].pts.contains(&stand_in));
    let site2 = c.site(2);
    c.a.apply(c.recv, c.recv_obj, &site2);
    let pending = queued && c.a.vars[inner].queued;
    c.a.solve();
    let site1 = c.site(1);
    c.a.constrain(c.recv, site1);
    c.a.solve();
    let memo_after = c.a.receiver_members.get(&(c.recv_obj, ArcStr::from("map"))).copied();
    Staged {
      outcome: c.p.outcome(&c.a),
      cached,
      pending,
      behind,
      members: members(&c.a),
      memo_after,
    }
  }

  /// Guard 3, a site applied while the wrapped values' widening is pending. The cached member
  /// still holds callee 0, which the widening dropped; a fresh member reads the wrapped values
  /// as they are now, the stand-in. Site 2 must see what a fresh member sees, and so must site 1,
  /// applied later: after the wrapped values widen, no site on the wrap shares a member again.
  #[test]
  fn shared_receiver_member_skips_pending_inner_widening() {
    let old = staged_pending_widening(true);
    let new = staged_pending_widening(false);
    assert!(old.pending && new.pending, "site 2 was applied before the widening propagated");
    assert!(new.cached.is_some() && new.behind, "a cached member that had not caught up");
    assert_eq!(old.outcome, new.outcome);
    // Shared: site 0's member, then one each for sites 2 and 1. The fresh construction also
    // applies site 0 twice (from the new constraint and the receiver's pending seed), each
    // time with a member of its own.
    assert_eq!((old.members, new.members), (4, 3), "no member shared after the widening");
    assert_eq!(new.memo_after, new.cached, "the memo entry is neither reused nor replaced");
    let sites: Vec<String> =
      new.outcome[1..=3].iter().map(|l| l.split(", slot-in").next().unwrap().to_string()).collect();
    assert_eq!(
      sites,
      [
        "site 0: member [\"callee0\", \"receiver\", \"service\", \"widened stand-in\"] (widened \
         false), handed over true, slot [\"receiver\", \"widened stand-in\"], widened false",
        "site 1: member [\"receiver\", \"widened stand-in\"] (widened false), handed over false, \
         slot [], widened false",
        "site 2: member [\"receiver\", \"widened stand-in\"] (widened false), handed over false, \
         slot [], widened false",
      ]
    );
  }

  /// What a named load's object gains after the load ran (`Obj::named_loads`).
  #[derive(Clone, Copy, Debug)]
  enum Late {
    /// A prototype holding the value under the name (`SetProto`).
    Proto,
    /// A namespace as the prototype, whose export of the name is lazy: its body seeds the value.
    LazyProto,
    /// A spread source holding the value under the name.
    Spread,
    /// A store under an unknown key: the elements, and an array's named slot.
    UnknownStore,
    /// A prototype holding the value under the name, with a class getter declared on it: the
    /// getter runs with the object as `this` (`Analysis::inherit`).
    GetterProto,
  }

  /// A load of `name` (`*`: an unknown key) on a fresh object of `kind`, registered before or after `late`,
  /// solving after each step or once. Returns what the load holds (how many values, and
  /// whether the service is one: a store under an unknown key may stand a value in for it,
  /// `admitted_values`), whether a lazy export ran and whether a getter ran on the object,
  /// labelled.
  fn late_case(kind: &str, late: Late, name: &str, load_first: bool, solve_each: bool) -> String {
    let mut a = Analysis::new();
    let service = a.alloc(ObjKind::Object, None);
    let obj_kind = match kind {
      "array" => ObjKind::Array,
      "closure" => {
        let [this, ret, yielded_fail, yielded, args] = std::array::from_fn(|_| a.var());
        let f = FnInfo::new(
          ModuleIdx::from_raw_unchecked(0),
          0,
          FnKind::Normal,
          [this, ret, yielded_fail, yielded, args],
        );
        ObjKind::Closure(a.fns.push(f))
      }
      "class" => ObjKind::Class { ctor: None, heritage: None, prototype: a.var() },
      _ => ObjKind::Object,
    };
    let obj = a.alloc(obj_kind, None);
    let recv = a.var();
    a.seed(recv, obj);
    let dst = a.var();
    let ran = a.var();
    let getter_this = a.var();
    let key = ArcStr::from(name);
    let load = |a: &mut Analysis| {
      let key = if name == "*" { Key::Unknown } else { Key::Name(key.clone()) };
      a.constrain(recv, Constraint::Load { key, dst });
    };
    let event = |a: &mut Analysis| match late {
      Late::Proto | Late::LazyProto | Late::GetterProto => {
        let proto = if let Late::Proto | Late::GetterProto = late {
          let proto = a.alloc(ObjKind::Object, None);
          let field = a.field(proto, &ArcStr::from(name));
          a.seed(field, service);
          if let Late::GetterProto = late {
            let target = a.var();
            a.seed(target, proto);
            a.constrain(target, Constraint::Accessor(Accessor { this: getter_this, setter: None }));
          }
          proto
        } else {
          let ns = a.alloc(ObjKind::Namespace, None);
          let [this, ret, yielded_fail, yielded, args] = std::array::from_fn(|_| a.var());
          let mut f = FnInfo::new(
            ModuleIdx::from_raw_unchecked(0),
            0,
            FnKind::Normal,
            [this, ret, yielded_fail, yielded, args],
          );
          f.pending.push(Pending::Seed(ran, service));
          let export = a.var();
          f.pending.push(Pending::Seed(export, service));
          let f = a.fns.push(f);
          let field = a.field(ns, &ArcStr::from(name));
          a.copy(export, field);
          a.lazy_exports.insert((ns, ArcStr::from(name)), f);
          ns
        };
        let src = a.var();
        a.seed(src, proto);
        a.constrain(recv, Constraint::SetProto(src));
      }
      Late::Spread => {
        let source = a.alloc(ObjKind::Object, None);
        let field = a.field(source, &ArcStr::from(name));
        a.seed(field, service);
        let src = a.var();
        a.seed(src, source);
        a.constrain(src, Constraint::SpreadInto(obj));
      }
      Late::UnknownStore => {
        let src = a.var();
        a.seed(src, service);
        a.constrain(recv, Constraint::Store { key: Key::Unknown, src });
      }
    };
    if load_first {
      load(&mut a);
    } else {
      event(&mut a);
    }
    if solve_each {
      a.solve();
    }
    if load_first {
      event(&mut a);
    } else {
      load(&mut a);
    }
    a.solve();
    format!(
      "load holds {} values, service {}, lazy export ran {}, getter ran {}",
      a.vars[dst].pts.len(),
      a.vars[dst].pts.contains(&service),
      a.vars[ran].pts.contains(&service),
      a.vars[getter_this].pts.contains(&obj)
    )
  }

  /// Guard: replay of late inputs (`Obj::named_loads`). A load registered before its object
  /// gains a prototype, a lazy namespace prototype, a prototype with a getter, a spread source
  /// or elements holds what a load registered after it does, on objects, arrays (a named and an
  /// index read), closures and classes, for a named and an unknown key, solving between the two
  /// or not. The lazy export and the getter run either way, and every case reaches the load.
  #[test]
  fn late_inputs_reach_earlier_named_loads() {
    let mut wrong = vec![];
    let mut unreached = vec![];
    for kind in ["object", "array", "closure", "class"] {
      for late in
        [Late::Proto, Late::LazyProto, Late::GetterProto, Late::Spread, Late::UnknownStore]
      {
        for name in ["handler", "0", "*"] {
          // A lazy export runs for a read that names it (`touch_namespace`), so an unknown key
          // reads nothing from a lazy namespace prototype, before or after.
          if name == "0" && kind != "array" || name == "*" && matches!(late, Late::LazyProto) {
            continue;
          }
          let after = late_case(kind, late, name, false, true);
          let ran = match late {
            Late::LazyProto => after.contains("lazy export ran true"),
            Late::GetterProto => after.contains("getter ran true"),
            _ => true,
          };
          if after.starts_with("load holds 0 values") || !ran {
            unreached.push(format!("{kind} {late:?} {name}: {after}"));
          }
          for solve_each in [true, false] {
            let before = late_case(kind, late, name, true, solve_each);
            if before != after {
              wrong.push(format!(
                "{kind} {late:?} {name} solve each {solve_each}: before {before} / after {after}"
              ));
            }
          }
        }
      }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert_eq!(unreached, Vec::<String>::new());
  }

  /// A step of a spread scenario (`spread_case`), on objects `0` to `2`.
  #[derive(Clone, Copy, Debug)]
  enum Step {
    /// A read of `handler` on object 0.
    Read,
    /// Object 0's own field `handler`, the destination of what spreads into it.
    TargetField,
    /// Object `.0` spread into object `.1`.
    Spread(usize, usize),
    /// Object `.0` gains `handler`, holding the service.
    Field(usize),
  }

  /// Runs `steps`, solving after each or once. Returns whether the read and object 0's field
  /// hold the service and which objects demand `handler`, and whether the named-load rows
  /// settle: spreading every edge again twice adds none the second time, and every row is
  /// counted as an entry.
  fn spread_case(steps: &[Step], solve_each: bool) -> String {
    let mut a = Analysis::new();
    let handler = ArcStr::from("handler");
    let service = a.alloc(ObjKind::Object, None);
    let objs: Vec<ObjId> = (0..3).map(|_| a.alloc(ObjKind::Object, None)).collect();
    let dst = a.var();
    let mut field = None;
    let spread = |a: &mut Analysis, from: usize, into: usize| {
      let src = a.var();
      a.seed(src, objs[from]);
      a.constrain(src, Constraint::SpreadInto(objs[into]));
    };
    for step in steps {
      match *step {
        Step::Read => {
          let recv = a.var();
          a.seed(recv, objs[0]);
          a.constrain(recv, Constraint::Load { key: Key::Name(handler.clone()), dst });
        }
        Step::TargetField => field = Some(a.field(objs[0], &handler)),
        Step::Spread(from, into) => spread(&mut a, from, into),
        Step::Field(i) => {
          let f = a.field(objs[i], &handler);
          a.seed(f, service);
        }
      }
      if solve_each {
        a.solve();
      }
    }
    a.solve();
    let rows = |a: &Analysis| -> usize {
      a.objs.iter().map(|o| o.named_loads.as_ref().map_or(0, |l| l.len())).sum()
    };
    let held = format!(
      "read {}, field {}, demand {:?}",
      a.vars[dst].pts.contains(&service),
      field.is_some_and(|f| a.vars[f].pts.contains(&service)),
      objs.iter().map(|o| a.objs[*o].demand.has("handler")).collect::<Vec<_>>(),
    );
    let mut settled = vec![];
    for _ in 0..2 {
      for step in steps {
        if let Step::Spread(from, into) = *step {
          spread(&mut a, from, into);
        }
      }
      a.solve();
      settled.push(rows(&a));
    }
    format!(
      "{held}, settled {}",
      settled[0] == settled[1] && a.total_named_loads == settled[1] as u64,
    )
  }

  /// Guard: a spread source's late inputs reach the reads and fields replayed into it
  /// (`spread_into`). A read of the target, or the target's own field, registered before a
  /// source arrives sees a field the source gains after it, through one spread or two, with the
  /// target spread into itself and in a two-object cycle: as when the read comes last or the
  /// field comes before the spread. Demand is the same either way, and the named-load rows
  /// settle, each counted as an entry. (A spread applied again after its target gained a field
  /// replays that field too, so the rows may grow once.)
  #[test]
  fn late_spread_inputs_reach_earlier_loads() {
    use Step::{Field, Read, Spread, TargetField};
    let cases: [(&str, &[Step], &[Step]); 6] = [
      ("late field", &[Read, Spread(1, 0), Field(1)], &[Spread(1, 0), Field(1), Read]),
      (
        "target field",
        &[TargetField, Spread(1, 0), Field(1)],
        &[TargetField, Field(1), Spread(1, 0)],
      ),
      (
        "two hops",
        &[Read, Spread(1, 0), Spread(2, 1), Field(2)],
        &[Spread(1, 0), Spread(2, 1), Field(2), Read],
      ),
      (
        "two hops, target field",
        &[TargetField, Spread(1, 0), Spread(2, 1), Field(2)],
        &[TargetField, Field(2), Spread(2, 1), Spread(1, 0)],
      ),
      (
        "self spread",
        &[Read, Spread(0, 0), Spread(1, 0), Field(1)],
        &[Spread(0, 0), Spread(1, 0), Field(1), Read],
      ),
      (
        "cycle",
        &[Read, Spread(1, 0), Spread(0, 1), Field(1)],
        &[Spread(1, 0), Spread(0, 1), Field(1), Read],
      ),
    ];
    let mut wrong = vec![];
    for (name, late, reference) in cases {
      let expected = spread_case(reference, true);
      if !expected.contains("true,") || !expected.ends_with("settled true") {
        wrong.push(format!("{name} reference: {expected}"));
      }
      for solve_each in [true, false] {
        let got = spread_case(late, solve_each);
        if got != expected {
          wrong.push(format!("{name} solve each {solve_each}: {got} / reference {expected}"));
        }
      }
    }
    assert_eq!(wrong, Vec::<String>::new());
  }

  /// What a spread queued and kept (`fan_out`).
  #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
  struct Fanned {
    entries: u64,
    replay_loads: u64,
    /// Constraints on every variable: each constraint queued once (`constrain`), so this
    /// bounds how many the queue held at its peak.
    constraints: usize,
    /// Registrations in `loads_by_name`.
    registrations: usize,
    rows: usize,
  }

  impl Fanned {
    fn of(a: &Analysis) -> Self {
      Fanned {
        entries: a.entries(),
        replay_loads: a.total_replay_loads,
        constraints: a.vars.iter().map(|v| v.constraints.len()).sum(),
        registrations: a.loads_by_name.values().map(FxHashSet::len).sum(),
        rows: a.objs.iter().map(|o| o.named_loads.as_ref().map_or(0, |l| l.len())).sum(),
      }
    }

    fn since(self, before: Self) -> Self {
      Fanned {
        entries: self.entries - before.entries,
        replay_loads: self.replay_loads - before.replay_loads,
        constraints: self.constraints - before.constraints,
        registrations: self.registrations - before.registrations,
        rows: self.rows - before.rows,
      }
    }
  }

  /// A target with `k` fields and `k` named reads, and `sources` empty objects in one variable,
  /// each with its holder, all solved. With `room` the entry cap is set that far above the
  /// entries just before the variable is spread into the target, so the spread's own batch is
  /// all the cap sees. Returns the analysis, the sources and what the spread added.
  fn fan_out(k: usize, sources: usize, room: Option<u64>) -> (Analysis, Vec<ObjId>, Fanned) {
    let mut a = Analysis::new();
    let target = a.alloc(ObjKind::Object, None);
    let recv = a.var();
    a.seed(recv, target);
    for i in 0..k {
      let name = ArcStr::from(format!("f{i}"));
      a.field(target, &name);
      let dst = a.var();
      a.constrain(recv, Constraint::Load { key: Key::Name(name), dst });
    }
    let objs: Vec<ObjId> = (0..sources).map(|_| a.alloc(ObjKind::Object, None)).collect();
    let src = a.var();
    for o in &objs {
      a.holder(*o);
      a.seed(src, *o);
    }
    a.solve();
    let before = Fanned::of(&a);
    if let Some(room) = room {
      a.set_entry_cap(before.entries + room);
    }
    a.constrain(src, Constraint::SpreadInto(target));
    a.solve();
    let added = Fanned::of(&a).since(before);
    (a, objs, added)
  }

  /// Guard: replay fan-out is bounded by the budget as it is queued (`spread_into`). One
  /// variable holding 16 empty sources, spread into a target with 8 fields and 8 reads, queues
  /// 16 loads on each source's holder, one per field and read, each an entry, and each load
  /// gives its source a row, also an entry, and its name as demand. With room for 10 more
  /// entries, set just before the spread, replay starts and stops within the spread's one
  /// batch: at most 11 loads queued and registered, no more rows than the room, and the solve
  /// stops keeping everything.
  #[test]
  fn named_load_fan_out_is_counted_and_bounded() {
    let (k, sources) = (8, 16);
    let (a, objs, added) = fan_out(k, sources, None);
    let rows: Vec<usize> =
      objs.iter().map(|o| a.objs[*o].named_loads.as_ref().map_or(0, |l| l.len())).collect();
    assert_eq!(rows, vec![2 * k; sources]);
    let demand: Vec<bool> =
      objs.iter().map(|o| (0..k).all(|i| a.objs[*o].demand.has(&format!("f{i}")))).collect();
    assert_eq!(demand, vec![true; sources]);
    let fanned = 2 * k * sources;
    assert_eq!(
      added,
      Fanned {
        entries: 2 * fanned as u64,
        replay_loads: fanned as u64,
        constraints: fanned + 1,
        registrations: fanned,
        rows: fanned,
      },
      "the loads and rows, and the spread itself"
    );
    assert!(!a.budget_exceeded);
    let room = 10;
    let (a, _, added) = fan_out(k, sources, Some(room));
    assert!(a.budget_exceeded, "the solve stops");
    assert!(added.replay_loads >= 1, "replay starts: {added:?}");
    assert!(
      added.replay_loads <= room + 1
        && added.constraints as u64 <= room + 2
        && added.registrations as u64 <= room + 1
        && added.rows as u64 <= room
        && added.entries <= room + 1,
      "{added:?}"
    );
  }

  /// Replay-induced widening, as in `deferred_late_arrivals` (`reversed.js:317`): a read of
  /// `pick` on a target holds the target's own `test/C` and, through a spread, `test/D` from a
  /// field the source gains after the spread, then a third value there widens it past a limit of
  /// two. The read comes first, so what the source holds reaches it through replay
  /// (`spread_into`), or last. A lookup through what the read holds uses the names as keys, and
  /// `test/F` arrives after it and after widening. Returns the read's state (widened, and what
  /// it holds), the decisions (which keys the lookup sees, and the demand), and where each
  /// arriving value is: held, or crossed the widening boundary (a name into `handed_names`,
  /// the object third value into `widen_escaped`).
  fn replay_widening(read_first: bool, opaque_third: bool) -> (String, String, String) {
    let mut a = Analysis::new();
    a.widen_limit = 2;
    let pick = ArcStr::from("pick");
    let names = ["test/C", "test/D", "test/F", "test/Other"];
    let provided: Vec<ObjId> = names
      .iter()
      .map(|n| {
        let p = a.alloc(ObjKind::Object, None);
        let v = a.key_value_var(&ArcStr::from(*n));
        a.seed(v, p);
        p
      })
      .collect();
    let [target, source] = [0, 1].map(|_| a.alloc(ObjKind::Object, None));
    // The third value: an object, or the opaque value of a set already widened.
    let third = if opaque_third {
      let wide = a.var();
      for _ in 0..3 {
        let o = a.alloc(ObjKind::Object, None);
        a.seed(wide, o);
      }
      a.solve();
      *a.pts(wide).iter().next().expect("the opaque value")
    } else {
      a.alloc(ObjKind::Object, None)
    };
    let [recv, read] = [0, 1].map(|_| a.var());
    a.seed(recv, target);
    let reads = |a: &mut Analysis| {
      a.constrain(recv, Constraint::Load { key: Key::Name(pick.clone()), dst: read });
    };
    if read_first {
      reads(&mut a);
      a.solve();
    }
    let own = a.field(target, &pick);
    let c = a.str("test/C");
    a.seed(own, c);
    let src = a.var();
    a.seed(src, source);
    a.constrain(src, Constraint::SpreadInto(target));
    a.solve();
    // The source's field is new after the spread.
    let late = a.field(source, &pick);
    let d = a.str("test/D");
    a.seed(late, d);
    a.solve();
    a.seed(late, third);
    a.solve();
    if !read_first {
      reads(&mut a);
      a.solve();
    }
    // The lookup's own result holds three keys' values: past the limit it would widen too.
    // The read stays widened (`widen`).
    a.widen_limit = 8;
    let [key, dst] = [0, 1].map(|_| a.var());
    a.copy(read, key);
    a.constrain(key, Constraint::KeyNameUse(KeyUse::Lookup(dst)));
    a.solve();
    // A name after widening and after the lookup, through the target's own field, which holds
    // two values: only the read is widened on its way.
    let f = a.str("test/F");
    a.seed(own, f);
    a.solve();
    let class = a.widened_class.expect("a set widened");
    let opaque = a.opaque(class);
    let label = |o: &ObjId| match &a.objs[*o].kind {
      _ if *o == opaque => "opaque".to_string(),
      ObjKind::Str(name) => name.to_string(),
      _ => "object".to_string(),
    };
    let mut held: Vec<String> = a.pts(read).iter().map(label).collect();
    held.sort();
    let mut kept: Vec<String> = ["test/C", "test/D", "test/F"]
      .map(|name| {
        let place = if a.pts(read).iter().any(|o| label(o) == name) {
          "held"
        } else if a.handed_names.contains(name) {
          "handed"
        } else {
          "lost"
        };
        format!("{name} {place}")
      })
      .to_vec();
    if !opaque_third {
      let place = if a.pts(read).contains(&third) {
        "held"
      } else if a.widen_escaped.contains(&third) {
        "crossed"
      } else {
        "lost"
      };
      kept.push(format!("object {place}"));
    }
    kept.push(format!("opaque held {}", a.pts(read).contains(&opaque)));
    (
      format!("widened {}, holds {held:?}", a.vars[read].widened),
      format!(
        "sees {:?}, demand {:?}",
        provided.iter().map(|p| a.pts(dst).contains(p)).collect::<Vec<_>>(),
        [target, source].map(|o| a.objs[o].demand.has("pick")),
      ),
      kept.join(", "),
    )
  }

  /// Guard: a read widened by what replay brings it keeps every value it was given, as one made
  /// last does (`widen`). In either order each name, and the object third value, is still held
  /// or crossed the widening boundary (a name into `handed_names`, for lookups registered before
  /// or after), and the opaque value is held. With an object as the third value the read widens
  /// either way and holds only the opaque value. With the opaque value as the third, the read
  /// made last does not widen (cascaded widening): the target's own field widened first and
  /// gives it the opaque value in place of `test/C`, so it holds exactly the opaque value and
  /// `test/D`, while the read made first widens. Copy edges alone do the same at `39fab47d`,
  /// before replay: widening counts values in arrival order (first come). Lookups through either
  /// state see the same keys and demand is the same here, but that is this shape's, not a
  /// general guarantee.
  #[test]
  fn replay_widening_matches_read_last() {
    let mut wrong = vec![];
    for opaque_third in [false, true] {
      let (last_state, last, last_kept) = replay_widening(false, opaque_third);
      if last != "sees [true, true, true, false], demand [true, true]" {
        wrong.push(format!("opaque {opaque_third} read last: {last}"));
      }
      let (first_state, first, first_kept) = replay_widening(true, opaque_third);
      if first != last {
        wrong.push(format!("opaque {opaque_third} read first: {first} / read last {last}"));
      }
      for (order, kept) in [("first", &first_kept), ("last", &last_kept)] {
        if kept.contains("lost") || !kept.ends_with("opaque held true") {
          wrong.push(format!("opaque {opaque_third} read {order}: {kept}"));
        }
      }
      let expected_last = if opaque_third {
        r#"widened false, holds ["opaque", "test/D"]"#
      } else {
        r#"widened true, holds ["opaque"]"#
      };
      if first_state != r#"widened true, holds ["opaque"]"# || last_state != expected_last {
        wrong.push(format!("opaque {opaque_third}: read first {first_state} / last {last_state}"));
      }
    }
    assert_eq!(wrong, Vec::<String>::new());
  }

  /// A service provided under `name`, and an object whose `key` field names it.
  fn provided_key(a: &mut Analysis, name: &str) -> (ObjId, ObjId) {
    let provided = a.alloc(ObjKind::Object, None);
    let v = a.key_value_var(&ArcStr::from(name));
    a.seed(v, provided);
    let keyed = a.alloc(ObjKind::Object, None);
    let key = a.field(keyed, &ArcStr::from("key"));
    let s = a.str(name);
    a.seed(key, s);
    (provided, keyed)
  }

  /// Looks a service up through what `var` holds and calls its method `m`: whether the
  /// service provided as `provided` is found, and whether its `m` is demanded.
  fn looked_up(a: &mut Analysis, var: VarId, provided: ObjId) -> String {
    let [service, method] = [0, 1].map(|_| a.var());
    a.constrain(var, Constraint::KeyUse(KeyUse::Lookup(service)));
    a.constrain(service, Constraint::Load { key: Key::Name(ArcStr::from("m")), dst: method });
    a.solve();
    format!("found {}, m {}", a.pts(service).contains(&provided), a.objs[provided].demand.has("m"))
  }

  const FOUND: &str = "found true, m true";

  /// The widening boundary's opaque value, from a set of three values past a limit of two:
  /// `first` and two objects when given, or three names. Only names have crossed then, so the
  /// opaque value has not crossed its own boundary (an object crossing seeds its fields with
  /// it, `class_escape`).
  fn widened_opaque(a: &mut Analysis, first: Option<ObjId>) -> ObjId {
    let saved = a.widen_limit;
    a.widen_limit = 2;
    let set = a.var();
    let mut objs = vec![];
    match first {
      Some(o) => {
        objs.push(o);
        while objs.len() < 3 {
          objs.push(a.alloc(ObjKind::Object, None));
        }
      }
      None => {
        for name in ["test/W1", "test/W2", "test/W3"] {
          objs.push(a.str(name));
        }
      }
    }
    for o in objs {
      a.seed(set, o);
    }
    a.solve();
    assert!(a.vars[set].widened, "setup: the set widens");
    a.widen_limit = saved;
    let class = a.widened_class.expect("setup: a widening class");
    a.opaque(class)
  }

  /// The receivers the prototype and spread-target probes run on: the widening boundary's
  /// opaque value (as a mixed set's, with a string beside it), code the pass can't see
  /// (`top_ext`), and an external value no class stands behind.
  #[derive(Clone, Copy, Debug)]
  enum OpaqueRecv {
    Widened,
    TopExt,
    External,
  }

  fn receiver(a: &mut Analysis, kind: OpaqueRecv) -> VarId {
    let r = a.var();
    let obj = match kind {
      OpaqueRecv::Widened => widened_opaque(a, None),
      OpaqueRecv::TopExt => a.top_ext,
      OpaqueRecv::External => {
        a.alloc(ObjKind::External { flavour: Flavour::Ext, name: ArcStr::from("probe") }, None)
      }
    };
    a.seed(r, obj);
    let d = a.str("test/D");
    a.seed(r, d);
    a.solve();
    assert!(!a.vars[r].widened, "setup: the receiver is a mixed set, not widened");
    r
  }

  /// `Object.getPrototypeOf` on a receiver: an object whose prototype names `test/Proto` crossed
  /// the receiver's boundary (into the widening, or to code the pass can't see), and a lookup
  /// through the receiver's prototype finds that service. Returns the lookup's result.
  fn proto_of(kind: OpaqueRecv) -> String {
    let mut a = Analysis::new();
    let (provided, proto) = provided_key(&mut a, "test/Proto");
    let obj = a.alloc(ObjKind::Object, None);
    let pv = a.proto_var(obj);
    a.seed(pv, proto);
    match kind {
      OpaqueRecv::Widened => {
        widened_opaque(&mut a, Some(obj));
      }
      OpaqueRecv::TopExt | OpaqueRecv::External => {
        let v = a.var();
        a.seed(v, obj);
        a.constrain(v, Constraint::Escape(Flavour::Ext, None));
        a.solve();
      }
    }
    assert!(a.handed_names.contains("test/Proto"), "setup: the prototype's name is handed");
    let r = receiver(&mut a, kind);
    let dst = a.var();
    a.constrain(r, Constraint::ProtoOf(dst));
    looked_up(&mut a, dst, provided)
  }

  /// `Object.setPrototypeOf` (`set_proto`) or a spread into the receiver (`Object.assign`) with a
  /// fresh object naming `test/Assigned`, never handed otherwise; then a lookup through the
  /// receiver. Returns the lookup's result.
  fn handed_into(kind: OpaqueRecv, set_proto: bool) -> String {
    let mut a = Analysis::new();
    let (provided, keyed) = provided_key(&mut a, "test/Assigned");
    let r = receiver(&mut a, kind);
    assert!(
      a.pts(r).iter().all(|o| !a.class_escaped.contains_key(o) && a.objs[*o].escaped.is_none()),
      "setup: the receiver has crossed no boundary"
    );
    assert!(!a.handed_names.contains("test/Assigned"), "setup: the name is fresh");
    let src = a.var();
    a.seed(src, keyed);
    a.constrain(
      r,
      if set_proto { Constraint::SetProto(src) } else { Constraint::SpreadTarget(src) },
    );
    a.solve();
    looked_up(&mut a, r, provided)
  }

  /// Guard: the prototype of the widening boundary's opaque value is that boundary's, so a
  /// lookup through it sees the names handed across, as through the value itself
  /// (`Constraint::ProtoOf`). A concrete receiver finds its prototype's key either way.
  #[test]
  fn proto_of_widened_opaque_sees_handed_keys() {
    let mut a = Analysis::new();
    let (provided, proto) = provided_key(&mut a, "test/Proto");
    let obj = a.alloc(ObjKind::Object, None);
    let pv = a.proto_var(obj);
    a.seed(pv, proto);
    let r = a.var();
    a.seed(r, obj);
    let dst = a.var();
    a.constrain(r, Constraint::ProtoOf(dst));
    assert_eq!(looked_up(&mut a, dst, provided), FOUND, "setup: a concrete receiver");
    assert_eq!(proto_of(OpaqueRecv::Widened), FOUND);
  }

  /// Guard: the same through replay. A read of `pick` on a target, made before (through replay)
  /// or after a spread source's late field brings two more values, widens past a limit of two
  /// and holds the boundary's opaque value in place of the target's own object, whose prototype
  /// names `test/Proto`. A prototype read registered after it settles still finds the service.
  #[test]
  fn proto_of_replay_widened_read_sees_handed_keys() {
    let mut wrong = vec![];
    for read_first in [true, false] {
      let mut a = Analysis::new();
      a.widen_limit = 2;
      let (provided, proto) = provided_key(&mut a, "test/Proto");
      let obj = a.alloc(ObjKind::Object, None);
      let pv = a.proto_var(obj);
      a.seed(pv, proto);
      let pick = ArcStr::from("pick");
      let [target, source] = [0, 1].map(|_| a.alloc(ObjKind::Object, None));
      let [recv, read] = [0, 1].map(|_| a.var());
      a.seed(recv, target);
      let reads = |a: &mut Analysis| {
        a.constrain(recv, Constraint::Load { key: Key::Name(pick.clone()), dst: read });
        a.solve();
      };
      if read_first {
        reads(&mut a);
      }
      let own = a.field(target, &pick);
      a.seed(own, obj);
      let src = a.var();
      a.seed(src, source);
      a.constrain(src, Constraint::SpreadInto(target));
      a.solve();
      let late = a.field(source, &pick);
      let d = a.str("test/D");
      a.seed(late, d);
      let e = a.str("test/E");
      a.seed(late, e);
      a.solve();
      if !read_first {
        reads(&mut a);
      }
      a.widen_limit = 400;
      assert!(!a.pts(read).contains(&obj), "setup: the read no longer holds the object");
      assert!(a.handed_names.contains("test/Proto"), "setup: the prototype's name is handed");
      let dst = a.var();
      a.constrain(read, Constraint::ProtoOf(dst));
      let found = looked_up(&mut a, dst, provided);
      if found != FOUND {
        wrong.push(format!("read first {read_first}: {found}"));
      }
    }
    assert_eq!(wrong, Vec::<String>::new());
  }

  /// Guard: a prototype set on the widening boundary's opaque value crosses into the boundary,
  /// as a value stored on it does, so a lookup through it sees the key the prototype names
  /// (`Constraint::SetProto`).
  #[test]
  fn set_proto_on_widened_opaque_crosses() {
    assert_eq!(handed_into(OpaqueRecv::Widened, true), FOUND);
  }

  /// Guard: an object spread into the widening boundary's opaque value crosses into the
  /// boundary, as a value stored on it does (`Constraint::SpreadTarget`).
  #[test]
  fn spread_into_widened_opaque_crosses() {
    assert_eq!(handed_into(OpaqueRecv::Widened, false), FOUND);
  }

  /// OpaqueRecv: the same three on code the pass can't see, and on an external value no class
  /// stands behind.
  #[test]
  fn plain_external_receivers_probe() {
    let mut wrong = vec![];
    for kind in [OpaqueRecv::TopExt, OpaqueRecv::External] {
      for (what, found) in [
        ("proto of", proto_of(kind)),
        ("set proto", handed_into(kind, true)),
        ("spread target", handed_into(kind, false)),
      ] {
        if found != FOUND {
          wrong.push(format!("{kind:?} {what}: {found}"));
        }
      }
    }
    assert_eq!(wrong, Vec::<String>::new());
  }

  /// How a late-input probe's target reaches code outside the analysis: to code the pass
  /// can't see (genuine), or only to effect's code (`EFFECT_ESCAPE`).
  #[derive(Clone, Copy, Debug, PartialEq)]
  enum Escaped {
    Genuine,
    Effect,
  }

  /// What is put on an escaped target, and when: a spread of a fresh source after the escape,
  /// a spread registered before it whose source value arrives after, and the controls (a
  /// named store after the escape, a spread before it, a store registered before it whose
  /// value arrives after).
  #[derive(Clone, Copy, Debug, PartialEq)]
  enum LateInput {
    SpreadAfter,
    SpreadThenValue,
    StoreAfter,
    SpreadBefore,
    StoreThenValue,
  }

  /// An empty object escapes and the solve drains; then a fresh source naming
  /// `test/Assigned` reaches it as `input` says. Code outside may read the source's `key`
  /// through the target and look the service up with it: returns that lookup's result
  /// (`looked_up` through `top_ext`).
  fn late_into_escaped(escaped: Escaped, input: LateInput) -> String {
    let mut a = Analysis::new();
    let (provided, keyed) = provided_key(&mut a, "test/Assigned");
    let target = a.alloc(ObjKind::Object, None);
    let [tv, src, esc] = [0, 1, 2].map(|_| a.var());
    a.seed(tv, target);
    let store = Constraint::Store { key: Key::Name(ArcStr::from("assigned")), src };
    match input {
      LateInput::SpreadBefore => {
        a.seed(src, keyed);
        a.constrain(tv, Constraint::SpreadTarget(src));
      }
      LateInput::SpreadThenValue => a.constrain(tv, Constraint::SpreadTarget(src)),
      LateInput::StoreThenValue => a.constrain(tv, store.clone()),
      LateInput::SpreadAfter | LateInput::StoreAfter => {}
    }
    a.solve();
    let site = (escaped == Escaped::Effect).then_some(EFFECT_ESCAPE);
    a.seed(esc, target);
    a.constrain(esc, Constraint::Escape(Flavour::Ext, site));
    a.solve();
    assert_eq!(a.objs[target].escaped, Some(Flavour::Ext), "setup: the target escaped");
    assert_eq!(
      a.genuine_escaped.contains(&target),
      escaped == Escaped::Genuine,
      "setup: genuine or effect-only"
    );
    if matches!(input, LateInput::SpreadAfter | LateInput::SpreadThenValue) {
      assert!(
        a.objs[target].fields.is_empty()
          && a.named_loads(target).is_empty()
          && a.objs[target].unknown_loads.is_empty()
          && a.crossed_classes(target).is_empty(),
        "setup: the target has no fields, no reads and crossed no class"
      );
      assert!(!a.handed_names.contains("test/Assigned"), "setup: the name is fresh");
      assert!(
        a.objs[keyed].escaped.is_none() && !a.class_escaped.contains_key(&keyed),
        "setup: the source has not escaped"
      );
    }
    match input {
      LateInput::SpreadAfter => {
        a.seed(src, keyed);
        a.constrain(tv, Constraint::SpreadTarget(src));
      }
      LateInput::StoreAfter => {
        a.seed(src, keyed);
        a.constrain(tv, store);
      }
      LateInput::SpreadThenValue | LateInput::StoreThenValue => a.seed(src, keyed),
      LateInput::SpreadBefore => {}
    }
    a.solve();
    let outside = a.var();
    let top = a.top_ext;
    a.seed(outside, top);
    looked_up(&mut a, outside, provided)
  }

  /// Guard: a source spread into an object after it escaped to code the pass can't see
  /// escapes with it, as a value stored on it does (`spread_into`, `store_name`).
  #[test]
  fn late_spread_into_escaped_target_escapes() {
    for control in [LateInput::StoreAfter, LateInput::SpreadBefore] {
      assert_eq!(late_into_escaped(Escaped::Genuine, control), FOUND, "control {control:?}");
    }
    assert_eq!(late_into_escaped(Escaped::Genuine, LateInput::SpreadAfter), FOUND);
  }

  /// Guard: the same after an escape only into effect's code.
  #[test]
  fn late_spread_into_effect_escaped_target_escapes() {
    for control in [LateInput::StoreAfter, LateInput::SpreadBefore] {
      assert_eq!(late_into_escaped(Escaped::Effect, control), FOUND, "control {control:?}");
    }
    assert_eq!(late_into_escaped(Escaped::Effect, LateInput::SpreadAfter), FOUND);
  }

  /// Guard: a spread registered before the target escaped, whose source value arrives after
  /// the escape drained, escapes with it.
  #[test]
  fn spread_source_arriving_after_escape_escapes() {
    for control in [LateInput::StoreThenValue, LateInput::SpreadBefore] {
      assert_eq!(late_into_escaped(Escaped::Genuine, control), FOUND, "control {control:?}");
    }
    assert_eq!(late_into_escaped(Escaped::Genuine, LateInput::SpreadThenValue), FOUND);
  }

  /// Guard: over the budget, replay and the named-load index stop (`replay_stopped`). A read,
  /// a late element, prototype or spread source adds no row, copy or constraint, and the solve
  /// stops keeping everything even after the totals come back under the cap, as widening may
  /// take them.
  #[test]
  fn replay_stops_over_budget() {
    let mut a = Analysis::new();
    // Read under two names, and under an unknown key.
    let [obj, unknown_read] = [0, 1].map(|_| a.alloc(ObjKind::Object, None));
    let [recv, unknown_recv, unknown] = [0, 1, 2].map(|_| a.var());
    a.seed(recv, obj);
    a.seed(unknown_recv, unknown_read);
    for name in ["a", "b"] {
      let dst = a.var();
      a.constrain(recv, Constraint::Load { key: Key::Name(ArcStr::from(name)), dst });
    }
    a.constrain(unknown_recv, Constraint::Load { key: Key::Unknown, dst: unknown });
    a.solve();
    let source = a.alloc(ObjKind::Object, None);
    let dst = a.var();
    a.set_entry_cap(a.entries() - 1);
    let work = |a: &Analysis| {
      (
        a.total_named_loads,
        a.total_copies,
        a.new_constraints.len(),
        a.vars.iter().map(|v| v.constraints.len()).sum::<usize>(),
      )
    };
    let before = work(&a);
    a.load_name(obj, &ArcStr::from("c"), dst);
    assert_eq!(work(&a).0, before.0, "a read over budget adds no row");
    let before = work(&a);
    a.elem(obj);
    assert_eq!(work(&a), before, "late elements replay nothing");
    a.proto_var(obj);
    a.proto_var(unknown_read);
    // Each prototype variable's own `Inherits`, and nothing replayed.
    assert_eq!(work(&a).3, before.3 + 2, "a late prototype replays nothing");
    let before = work(&a);
    a.spread_into(source, obj);
    assert_eq!(work(&a).2, before.2, "a late spread source replays nothing");
    a.set_entry_cap(u64::MAX);
    assert!(!a.over_budget());
    a.solve();
    assert!(a.budget_exceeded, "the solve stops");
  }
}
