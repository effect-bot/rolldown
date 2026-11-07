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
const RULE_SUB: u32 = 0x4000_0000;

/// The model slots of a Fiber: what its effect succeeds and fails with.
pub const FIBER_SUCCESS: &str = "~success";
pub const FIBER_FAILURE: &str = "~failure";
/// Tag of the call sites inside a function clone (both bits); the low bits index `clone_sites`.
const CLONE_SUB: u32 = 0xC000_0000;

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

/// A row of the debug callback census: (module, site, mask) to (runs, largest slot).
type CallbackRow = ((u32, u32, u8), (usize, usize));

/// `ObjKind::Typed::export` of a value that is not an export.
pub const NO_EXPORT: u32 = u32::MAX;

/// An export of a type-modelled `effect` module: its `.d.ts` (if any), export name and module.
pub struct TypedExport {
  pub dts: Option<std::path::PathBuf>,
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
  Call(Box<Call>),
  SuccessOf(VarId),
  FailureOf(VarId),
  KeyUse(KeyUse),
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
  /// contributes to the call's slot.
  TypedArg(VarId, VarId, Site, u8),
  /// Values entering a type-modelled value's slot (`Analysis::slot_in`); the object is the
  /// stand-in for what the admission filter holds back.
  SlotIn(VarId, ObjId),
  /// A call reaching only the functions (closures, classes, builtins) of the variable.
  CallFn(Box<Call>),
  /// A protocol method call (`protocol_call`) with the type model on: a summary found under a
  /// protocol name is effect code run with the receiver as `this`, so the receiver goes to
  /// unseen code instead of the summary being called; anything else is called.
  ProtocolCall(Box<Call>),
  /// What a callback handed to type-modelled code returns (`typed_callback`): it goes to unseen
  /// code, and so does what an Effect or promise among it succeeds with.
  EscapeOut,
  /// A member of an options object handed to type-modelled code: a function effect code may
  /// call with the slot (`typed_callback`); a type-modelled function among them is routed as
  /// `pool_route` does.
  TypedNested(VarId, VarId, Site, u8),
  /// Direction 2's admission filter at a type-modelled call (`Analysis::admit_typed`).
  AdmitTyped(VarId),
  /// An argument at a concrete position of a call into type-modelled code (its declared type,
  /// interned, mentions no type parameter; `u32::MAX` if unknown): it joins no slot, but its
  /// callbacks run and its declared members are read (`Analysis::typed_concrete`).
  TypedConcrete(u32, Site),
  /// Every key named by these strings keeps all its methods (a key handed to type-modelled
  /// code, which may provide or read it).
  BailKeys,
  /// A callable reaching an adopted object's `then` (`Analysis::adopt`): the object bails.
  ThenableBail(ObjId),
  /// A callable reaching an adopted object's `then`: the await yields an unknown value.
  ThenableAwait(VarId),
  /// What may enter a Schedule (issue, "Assumptions" 4): primitives and ordinary values flow
  /// into the variable with their structure, a program service value never does, and a summary
  /// becomes an unknown scalar (`Analysis::service_free`).
  ServiceFree(VarId),
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
      C::Call(call) => C::Call(Box::new(Call {
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
      C::ServiceFree(dst) => C::ServiceFree(v(*dst)),
      C::ThenableBail(obj) => C::ThenableBail(*obj),
      C::ThenableAwait(dst) => C::ThenableAwait(v(*dst)),
      C::TypedArg(slot, back, site, mask) => C::TypedArg(v(*slot), v(*back), *site, *mask),
      C::SlotIn(dst, stand_in) => C::SlotIn(v(*dst), *stand_in),
      C::TypedConcrete(ty, site) => C::TypedConcrete(*ty, *site),
      C::AdmitTyped(dst) => C::AdmitTyped(v(*dst)),
      C::TypedNested(slot, back, site, mask) => C::TypedNested(v(*slot), v(*back), *site, *mask),
      C::EscapeOut => C::EscapeOut,
      C::ProtocolCall(call) => match C::Call(call.clone()).remap(v, s) {
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
      C::LookupIn { key, dst } => C::LookupIn { key: v(*key), dst: v(*dst) },
      C::Accessor(acc) => C::Accessor(Accessor {
        this: v(acc.this),
        setter: acc.setter.as_ref().map(|(name, param)| (name.clone(), v(*param))),
      }),
      C::Inherits(_)
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
}

#[derive(Default)]
pub struct Var {
  pub pts: FxHashSet<ObjId>,
  pub copies: FxHashSet<VarId>,
  pub constraints: Vec<Constraint>,
  /// The constraints above, for deduplication: a constraint registered twice would re-fire
  /// on every object and could never settle in a prototype or spread cycle.
  constraint_set: FxHashSet<Constraint>,
  delta: Vec<ObjId>,
  queued: bool,
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
  /// Type bounds of exported functions of `effect`'s ordinary modules, by (module, span start):
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
  /// (result, receiver) pairs of method calls already joined (`typed_call`).
  typed_receivers: FxHashSet<(ObjId, ObjId)>,
  /// Each call's handed-values variable, translated for slots (`slot_in`).
  typed_backs: FxHashMap<VarId, VarId>,
  /// (object, slot) pairs `typed_arg` has handled.
  typed_args_seen: FxHashSet<(ObjId, VarId)>,
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
  method_only: FxHashMap<ModuleIdx, std::rc::Rc<FxHashSet<String>>>,
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
  /// Objects known to hold something that may be a service (`may_hold` only grows).
  live: FxHashSet<ObjId>,
  /// The size of `never_prune` when `live` was last valid.
  live_never_prune: usize,
  /// Objects known to reach code (`reaches_code` only grows).
  live_code: FxHashSet<ObjId>,
  /// Admission counters for the report: admitted, held back, tainted stand-ins.
  pub admission_stats: (u64, u64, u64),
  /// Direction 2's admission lever on the protocol walks, host adoption and unknown-key stores.
  /// On with the type model (`ROLLDOWN_EFFECT_SERVICE_SHAKING_TYPEMODEL=1`), which is opt-in.
  pub admission: bool,
  /// What awaiting each summary gives (`AwaitOf` on a `Wrap`).
  awaited_wraps: FxHashMap<ObjId, VarId>,
  /// Each adopted object's `then` (`adopt`).
  thenable_then: FxHashMap<ObjId, VarId>,
  /// Hand-written thenables that have bailed (`adopt`).
  thenables_bailed: FxHashSet<ObjId>,

  /// The value variables of providers (a `KeyUse::Provide` that reached a key), for
  /// `definitely_service`.
  provide_sources: FxHashSet<VarId>,
  /// Objects the generator proves are allocated at most once at run time (`Gen::alloc`).
  pub once_allocated: FxHashSet<ObjId>,
  /// `ServiceFree` objects waiting for the solve to settle (`service_free`).
  pending_service_free: Vec<(ObjId, VarId)>,
  prim_ty: Option<u32>,
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
  /// The values `Cause.done` completes a Pull with: a Schedule's output when it ends. They are
  /// success-side data that Effect carries through the error channel, so they are the one thing
  /// handlers receive (`done_failure`).
  pub done_values: VarId,
  /// The Done signal handlers see in place of a failure: a Cause whose one Fail reason's error is
  /// a `Done` with `done_values` as its value, and that `Done` itself (built once).
  done_failure: Option<(ObjId, ObjId)>,
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
  pub top_host: ObjId,
  pub prim: ObjId,
  pub all_keys: ObjId,
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
  /// The bundled `Schedule.toStep` (stable public API), which the `Effect.repeat`,
  /// `Effect.retry` and `Effect.retryOrElse` rules use to step a Schedule. `None` when
  /// `effect/Schedule` is not bundled.
  pub schedule_to_step: Option<VarId>,
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
    let top_host = alloc(ObjKind::Top(Flavour::Host));
    let prim = alloc(ObjKind::Prim);
    let all_keys = alloc(ObjKind::AllKeys);
    let ambient_keys = vars.push(Var::default());
    let ambient_context = alloc(ObjKind::Context { keys: ambient_keys });
    let unknown_key_values = vars.push(Var::default());
    let all_values = vars.push(Var::default());
    let failure_sink = vars.push(Var::default());
    let done_values = vars.push(Var::default());
    let effect_key_values = vars.push(Var::default());
    let effect_key_strings = vars.push(Var::default());
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
      walked: FxHashSet::default(),
      ignored_pool: None,
      prim_arg: None,
      inspect_args: None,
      pending_service_free: vec![],
      once_allocated: FxHashSet::default(),
      provide_sources: FxHashSet::default(),
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
      live: FxHashSet::default(),
      live_code: FxHashSet::default(),
      live_never_prune: 0,
      admission_stats: (0, 0, 0),
      admission: false,
      typed_args_seen: FxHashSet::default(),
      typed_backs: FxHashMap::default(),
      typed_receivers: FxHashSet::default(),
      typed_slot_fns: FxHashSet::default(),
      prim_ty: None,
      hash_pool: None,
      json_pool: None,
      inspect_pool: None,
      primitive_converted: FxHashSet::default(),
      log_messages: None,
      failure_sink,
      done_values,
      done_failure: None,
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
      top_host,
      prim,
      all_keys,
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
      schedule_to_step: None,
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
    };
    this.seed(ambient_keys, all_keys);
    // Unknown keys alias every key: what they provide is visible through every key.
    this.copy(unknown_key_values, all_values);
    this
  }

  // == construction

  pub fn var(&mut self) -> VarId {
    self.vars.push(Var::default())
  }

  pub fn alloc(&mut self, kind: ObjKind, site: Option<Site>) -> ObjId {
    self.objs.push(Obj::new(kind, site))
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
    // Lookups through unknown keys see every key, and every key sees what unknown keys provide.
    self.copy(v, self.all_values);
    self.copy(self.unknown_key_values, v);
    if self.effect_keys.contains(key) {
      self.copy(v, self.effect_key_values);
    }
    v
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
    let stores = self.objs[obj].unknown_stores.clone();
    for src in stores {
      self.copy(src, v);
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

  pub fn elem(&mut self, obj: ObjId) -> VarId {
    if let Some(v) = self.objs[obj].elem {
      return v;
    }
    let v = self.var();
    self.objs[obj].elem = Some(v);
    v
  }

  pub fn proto_var(&mut self, obj: ObjId) -> VarId {
    if let Some(v) = self.objs[obj].proto {
      return v;
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

  pub fn seed(&mut self, var: VarId, obj: ObjId) {
    if self.vars[var].pts.insert(obj) {
      self.total_pts += 1;
      self.vars[var].delta.push(obj);
      self.enqueue(var);
    }
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
    if !self.vars[var].constraint_set.insert(constraint.clone()) {
      return;
    }
    match &constraint {
      Constraint::Load { key: Key::Name(name), dst } => {
        self.loads_by_name.entry(name.clone()).or_default().insert((var, *dst));
      }
      Constraint::LoadThrough { base, dst } => {
        // The key is dynamic: register under every name once it is known (see `load_through`).
        let _ = (base, dst);
      }
      _ => {}
    }
    self.vars[var].constraints.push(constraint);
    let index = self.vars[var].constraints.len() - 1;
    self.new_constraints.push((var, index));
  }

  fn enqueue(&mut self, var: VarId) {
    if !self.vars[var].queued {
      self.vars[var].queued = true;
      self.worklist.push(var);
    }
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

  fn over_budget(&self) -> bool {
    self.objs.len() > self.max_objects()
      || self.vars.len() > self.max_vars()
      || self.total_pts + self.total_copies
        > std::env::var("ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_ENTRIES")
          .ok()
          .and_then(|s| s.parse().ok())
          .unwrap_or(Self::MAX_ENTRIES)
  }

  fn max_objects(&self) -> usize {
    std::env::var("ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_OBJECTS")
      .ok()
      .and_then(|s| s.parse().ok())
      .unwrap_or(Self::MAX_OBJECTS)
  }

  fn max_vars(&self) -> usize {
    std::env::var("ROLLDOWN_EFFECT_SERVICE_SHAKING_MAX_VARS")
      .ok()
      .and_then(|s| s.parse().ok())
      .unwrap_or(Self::MAX_VARS)
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

  pub fn solve(&mut self) {
    loop {
      if self.over_budget() {
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
          if self.recheck_all() | self.admit_service_free() | self.recheck_deferred() {
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
    if !self.stand_ins.is_empty() && self.stand_in_use(obj, c) {
      return;
    }
    match c {
      Constraint::Load { key, dst } => self.load(obj, key, *dst),
      Constraint::Store { key, src } => self.store(obj, key, *src),
      Constraint::LoadThrough { base, dst } => self.load_through(obj, *base, *dst),
      Constraint::StoreThrough { base, src } => self.store_through(obj, *base, *src),
      Constraint::Call(call) => self.call(obj, call),
      Constraint::SuccessOf(dst) => self.success_of(obj, *dst),
      Constraint::FailureOf(dst) => self.failure_of(obj, *dst),
      Constraint::KeyUse(use_) => self.key_use(obj, use_),
      Constraint::KeyHeritage(class, use_) => self.key_heritage(obj, *class, use_),
      Constraint::SubclassHeritage(use_) => self.subclass_heritage(obj, use_),
      Constraint::ContextKeys(dst) => match self.objs[obj].kind {
        ObjKind::Context { keys } | ObjKind::Layer { keys } | ObjKind::Runtime { keys } => {
          self.copy(keys, *dst);
        }
        ObjKind::Top(_) | ObjKind::Wrap(_) | ObjKind::Typed { .. } => {
          self.seed(*dst, self.all_keys);
        }
        ObjKind::Builtin { .. } if self.is_default(obj) => self.copy(self.effect_key_strings, *dst),
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
          if !self.admission || self.reaches_code(obj) {
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
      Constraint::SpreadTarget(src) => self.constrain(*src, Constraint::SpreadInto(obj)),
      Constraint::ProtoOf(dst) => {
        if matches!(
          self.objs[obj].kind,
          ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Class { .. }
        ) {
          let pv = self.proto_var(obj);
          self.copy(pv, *dst);
        }
      }
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
        ObjKind::Top(_) | ObjKind::External { .. } | ObjKind::Wrap(_) | ObjKind::Typed { .. } => {
          self.seed(*dst, self.all_keys);
        }
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
      Constraint::ServiceFree(dst) => self.service_free(obj, *dst),
      Constraint::ThenableBail(thenable) => {
        if Self::callable_kind(&self.objs[obj].kind) && self.thenables_bailed.insert(*thenable) {
          self.escape(*thenable, Flavour::Ext, None);
        }
      }
      Constraint::ThenableAwait(dst) => {
        if Self::callable_kind(&self.objs[obj].kind) {
          self.seed(*dst, self.top_ext);
        }
      }
      Constraint::TypedArg(slot, back, site, mask) => {
        self.typed_arg(obj, *slot, *back, *site, *mask);
      }
      Constraint::SlotIn(dst, stand_in) => self.slot_in(obj, *dst, *stand_in),
      Constraint::AdmitTyped(dst) => self.admit_typed(obj, *dst),
      Constraint::TypedConcrete(ty, site) => self.typed_concrete(obj, *ty, *site),
      Constraint::CallFn(call) => self.call_fn_only(obj, call),
      Constraint::ProtocolCall(call) => self.protocol_method_call(obj, call),
      Constraint::EscapeOut => match self.objs[obj].kind {
        ObjKind::Effect { success, .. } => self.constrain(success, Constraint::EscapeOut),
        ObjKind::Promise { value } => self.constrain(value, Constraint::EscapeOut),
        _ => self.escape(obj, Flavour::Ext, None),
      },
      Constraint::TypedNested(slot, back, site, mask) => {
        self.typed_nested(obj, *slot, *back, *site, *mask);
      }
      Constraint::BailKeys => self.bail_keys(obj),
      Constraint::LookupIn { key, dst } => self.lookup_in(obj, *key, *dst),
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
      }
    }
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

  fn store_through(&mut self, key: ObjId, base: VarId, src: VarId) {
    let bases: Vec<ObjId> = self.vars[base].pts.iter().copied().collect();
    match self.objs[key].kind.clone() {
      ObjKind::Str(name) => {
        for b in bases {
          self.store_name(b, &name, src);
        }
      }
      _ => {
        for b in bases {
          self.store_unknown(b, src);
        }
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
        if matches!(self.objs[obj].kind, ObjKind::Array) {
          if let Some(id) = self.rule_id(&format!("Array.prototype.{name}")) {
            let b = self.method(id, obj);
            self.seed(dst, b);
          }
        }
        self.read_field(obj, name, dst);
        if let Some(elem) = self.objs[obj].elem {
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
      ObjKind::Top(flavour) => {
        let t = if flavour == Flavour::Ext { self.top_ext } else { self.top_host };
        self.seed(dst, t);
      }
      ObjKind::External { flavour, name: ref module } => {
        if let Some(f) = self.objs[obj].fields.get(name).copied() {
          self.copy(f, dst);
          return;
        }
        let full = format!("{module}.{name}");
        if let Some(id) = self.rule_id(&full) {
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
          self.constrain(src, Constraint::Escape(Flavour::Ext, None));
        }
        let field = self.field(obj, name);
        self.copy(src, field);
      }
      ObjKind::Top(flavour) | ObjKind::External { flavour, .. } => {
        self.constrain(src, Constraint::Escape(flavour, None));
      }
      ObjKind::Typed { .. } if self.is_stand_in(obj) => {
        self.taint_by(obj, &format!("store {name}"));
      }
      // A value written onto a summary joins what it holds through the admission filter.
      ObjKind::Typed { slot: inner, .. } if self.admission => {
        self.constrain(src, Constraint::AdmitTyped(inner));
      }
      ObjKind::Wrap(inner) | ObjKind::Typed { slot: inner, .. } => self.copy(src, inner),
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
          self.constrain(src, Constraint::Escape(Flavour::Ext, None));
        }
        // Direction 2's admission lever: a value stored under an unknown name reaches every
        // field, so only what may hold a service flows as itself; the rest is stood for.
        let src = if self.admission { self.admitted_store(src) } else { src };
        let elem = self.elem(obj);
        self.copy(src, elem);
        if !self.objs[obj].unknown_stores.contains(&src) {
          self.objs[obj].unknown_stores.push(src);
          self.recheck_tree(obj);
        }
        let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
        for f in fields {
          self.copy(src, f);
        }
      }
      ObjKind::Top(flavour) | ObjKind::External { flavour, .. } => {
        self.constrain(src, Constraint::Escape(flavour, None));
      }
      ObjKind::Typed { .. } if self.is_stand_in(obj) => self.taint_by(obj, "unknown store"),
      // A value written onto a summary joins what it holds through the admission filter.
      ObjKind::Typed { slot: inner, .. } if self.admission => {
        self.constrain(src, Constraint::AdmitTyped(inner));
      }
      ObjKind::Wrap(inner) | ObjKind::Typed { slot: inner, .. } => self.copy(src, inner),
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
    for v in own.iter().chain(&locals).chain(&params).chain(rest.iter()) {
      if !map.contains_key(v) {
        let nv = self.var();
        map.insert(*v, nv);
      }
    }
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
        self.constrain(h, Constraint::Call(Box::new(parent_call)));
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

  /// `ServiceFree`: a primitive flows on; a summary (which may stand for a service) becomes an
  /// unknown scalar; an object waits for the solve to settle, then flows on unless it is
  /// definitely a service (`definitely_service`). Admitting an object is always sound; holding
  /// one back is sound only while it is definitely a service, so a held object is re-checked
  /// every time the solve settles (points-to sets keep growing after admissions) and flows as
  /// soon as it no longer is.
  fn service_free(&mut self, obj: ObjId, dst: VarId) {
    match self.objs[obj].kind {
      ObjKind::Str(_) | ObjKind::Num(_) | ObjKind::Prim => self.seed(dst, obj),
      ObjKind::Wrap(_) | ObjKind::Typed { .. } | ObjKind::Top(_) | ObjKind::External { .. } => {
        self.seed(dst, self.top_host);
      }
      _ => self.pending_service_free.push((obj, dst)),
    }
  }

  /// Admits the waiting `ServiceFree` objects that are not definitely services, keeping the rest
  /// waiting for the next check; returns whether any flowed.
  fn admit_service_free(&mut self) -> bool {
    let pending = std::mem::take(&mut self.pending_service_free);
    let mut admitted = false;
    let mut held = vec![];
    for (obj, dst) in pending {
      if self.vars[dst].pts.contains(&obj) {
        continue;
      }
      if self.definitely_service(obj) {
        held.push((obj, dst));
      } else {
        self.seed(dst, obj);
        admitted = true;
      }
    }
    self.pending_service_free = held;
    admitted
  }

  /// Must-provision: the object stands for one runtime object (`once_allocated`), and some
  /// provider's value holds exactly that object (`provide_sources`), so that runtime object is
  /// what was provided. Being among a key's possible values (`all_values`) is not enough: a
  /// provider's value may hold several objects (array elements merge, say), and then none of
  /// them is known to be the provided one.
  fn definitely_service(&self, obj: ObjId) -> bool {
    self.once_allocated.contains(&obj)
      && self.provide_sources.iter().any(|src| {
        let pts = &self.vars[*src].pts;
        pts.len() == 1 && pts.contains(&obj)
      })
  }

  /// What may enter a Schedule from `src` flows into `dst` (`Constraint::ServiceFree`).
  pub fn service_free_of(&mut self, src: VarId, dst: VarId) {
    self.constrain(src, Constraint::ServiceFree(dst));
  }

  /// The primitives `src` holds flow into `dst`; objects don't, and a summary becomes an
  /// unknown scalar (`Constraint::FilterCopy` with a primitive-only type).
  pub fn primitives_of(&mut self, src: VarId, dst: VarId) {
    let ty = match self.prim_ty {
      Some(t) => t,
      None => {
        let t = self.intern_ty(super::dts::Ty { prim: true, ..super::dts::Ty::default() });
        self.prim_ty = Some(t);
        t
      }
    };
    self.constrain(src, Constraint::FilterCopy { dst, ty });
  }

  /// An export of a type-modelled `effect` module (design section 11): its body is not
  /// analysed; calls take their flow from its `.d.ts` signature (`typed_call`).
  pub fn typed_export(
    &mut self,
    dts: Option<std::path::PathBuf>,
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
    let sig = match (&e.dts, outer) {
      (Some(dts), Some(outer)) => {
        let (dts, name) = (dts.clone(), e.name.clone());
        self.dts.returned_call(&dts, &name, origin.1, nargs).map(
          |(b, ret, (generic, rest_generic), masks)| {
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
            }
          },
        )
      }
      _ => None,
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
    let sig = match &e.dts {
      Some(dts) => {
        let (dts, name) = (dts.clone(), e.name.clone());
        let b = self.dts.bounds(&dts, &name, nargs);
        let ret = self.dts.ret_shape(&dts, &name, nargs);
        let (stores, reads) = self.dts.container_shape(&dts, &name, nargs);
        let (generic, rest_generic) =
          self.dts.generic_positions(&dts, &name, nargs).unwrap_or((vec![true; nargs], true));
        let masks =
          self.dts.callback_masks(&dts, &name, nargs).unwrap_or_else(|| vec![0xFF; nargs]);
        let params = b.params.into_iter().map(|t| t.map(|t| self.intern_ty(t))).collect();
        let rest = b.rest.map(|t| self.intern_ty(t));
        TypedSig { params, rest, ret, stores, reads, generic, rest_generic, masks }
      }
      None => TypedSig {
        params: vec![],
        rest: None,
        ret: super::dts::RetShape { unknown: true, ..Default::default() },
        stores: true,
        reads: true,
        generic: vec![true; nargs],
        rest_generic: true,
        masks: vec![0xFF; nargs],
      },
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
          Some(s) => std::rc::Rc::clone(s),
          None => {
            let path =
              self.typed_exports.iter().find(|e| e.module == m).and_then(|e| e.dts.clone());
            let set =
              std::rc::Rc::new(path.map(|p| self.dts.method_only_names(&p)).unwrap_or_default());
            self.method_only.insert(m, std::rc::Rc::clone(&set));
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
    let stand_in = self.top_ext;
    if self.debug {
      let e = self.typed_call_counts.entry((call.site.module.raw(), call.site.start)).or_default();
      e.0 += 1;
      e.1.insert(callee.raw());
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
        self.seed(slot, self.top_ext);
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
        // A method call has no signature: it bails what it reaches (below).
        self.constrain(this, Constraint::TypedArg(slot, back, call.site, 0));
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
    // the services it reaches: what it holds and is handed goes to unseen code.
    if sig.is_none() && !call.is_new {
      self.constrain(slot, Constraint::Escape(Flavour::Ext, None));
      self.constrain(handed, Constraint::Escape(Flavour::Ext, None));
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
          self.constrain(handed, Constraint::Escape(Flavour::Ext, None));
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
        self.constrain(arg, Constraint::AdmitTyped(handed));
        // Without a signature, the call hands what it holds to unseen code (below), so its
        // callbacks run with unknown arguments.
        let mask = sig.as_ref().map_or(0, |s| s.masks.get(i).copied().unwrap_or(0xFF));
        self.constrain(arg, Constraint::TypedArg(slot, back, call.site, mask));
      } else {
        self.constrain(arg, Constraint::TypedConcrete(ty.unwrap_or(u32::MAX), call.site));
      }
      // Effect's code may call a value's implicit protocol methods (the walk skips values that
      // reach no callable).
      self.copy(arg, pool);
    }
    let ret = sig.map_or(super::dts::RetShape { unknown: true, ..Default::default() }, |s| s.ret);
    let plain = if ret.concrete || ret.effect_concrete {
      let p = match self.typed_plain_results.get(&key) {
        Some(p) => *p,
        None => {
          let empty = self.var();
          let p = self.alloc(ObjKind::Typed { slot: empty, export: NO_EXPORT }, Some(call.site));
          self.typed_plain_results.insert(key, p);
          if let Some(m) = module {
            self.typed_result_module.insert(p, m);
          }
          if export != NO_EXPORT {
            self.typed_result_origin.insert(p, (export, nargs));
          }
          p
        }
      };
      Some(p)
    } else {
      None
    };
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
      self.seed(call.ret, eff);
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
  /// held-back object is re-checked whenever the solve settles (`recheck_deferred`).
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
    let mut seen: FxHashSet<ObjId> = FxHashSet::default();
    let mut stack = vec![obj];
    let mut vars: Vec<VarId> = vec![];
    while let Some(o) = stack.pop() {
      if !seen.insert(o) {
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
      if known.contains(&o) || provided || seen.len() > LIMIT {
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
          if code {
            self.live_code.insert(obj)
          } else {
            self.live.insert(obj)
          };
          return true;
        }
      }
      for v in vars.drain(..) {
        stack.extend(self.vars[v].pts.iter().copied());
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
      self.constrain(v, Constraint::TypedArg(returns, returns, call.site, 0xFF));
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
    let path = self.typed_exports.iter().find(|e| e.module == m).and_then(|e| e.dts.clone());
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
    if matches!(
      self.objs[obj].kind,
      ObjKind::Str(_) | ObjKind::Num(_) | ObjKind::Prim | ObjKind::AllKeys
    ) {
      return;
    }
    if self.may_hold(obj) {
      self.admission_stats.0 += 1;
      self.seed(dst, obj);
      return;
    }
    self.admission_stats.1 += 1;
    match self.objs[obj].kind {
      ObjKind::Object | ObjKind::Array | ObjKind::Closure(_) | ObjKind::Builtin { .. } => {
        self.escape(obj, Flavour::Ext, None);
      }
      // A summary, Effect or promise that holds no prunable service yet may come to hold one
      // (its slot, success or value grows): what it holds goes to unseen code as it arrives, so
      // a service reaching it later bails.
      ObjKind::Typed { .. } | ObjKind::Wrap(_) => self.escape(obj, Flavour::Ext, None),
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
  /// What a host builtin that copies structure hands out from `src` (direction 2's lever on
  /// builtin copies, with the type model on): the values that may hold a prunable service as
  /// themselves, the rest sent to unseen code and stood for by `top_ext` (`admitted_store`).
  /// Without the type model, `src` itself.
  pub fn admitted_values(&mut self, src: VarId) -> VarId {
    if self.admission { self.admitted_store(src) } else { src }
  }

  fn admitted_store(&mut self, src: VarId) -> VarId {
    if let Some(v) = self.unknown_store_filters.get(&src) {
      return *v;
    }
    let v = self.var();
    self.unknown_store_filters.insert(src, v);
    self.seed(v, self.top_ext);
    self.constrain(src, Constraint::AdmitTyped(v));
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
  /// (a Schedule's step, a stored callback); a stand-in enters as itself; anything else, a
  /// summary included, goes through the admission filter.
  fn slot_in(&mut self, obj: ObjId, dst: VarId, stand_in: ObjId) {
    match self.objs[obj].kind {
      ObjKind::Effect { success, .. } => {
        self.constrain(success, Constraint::SlotIn(dst, stand_in));
      }
      ObjKind::Promise { value } => self.constrain(value, Constraint::SlotIn(dst, stand_in)),
      // A function a type-modelled value holds may be called by its code (a Schedule's step, a
      // stored callback): it runs with what the value holds.
      ObjKind::Closure(_) | ObjKind::Builtin { .. } => {
        self.admit_typed(obj, dst);
        if self.typed_slot_fns.insert((obj, dst)) {
          let back = self.var();
          self.typed_callback(obj, dst, back, protocol_site(6), 0xFF);
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
  /// callbacks, and a record's or array's handlers do too (effect code reads them by name).
  /// The value itself enters the slot only if it may hold a service (`admit_typed`).
  fn typed_arg(&mut self, obj: ObjId, slot: VarId, back: VarId, site: Site, mask: u8) {
    // Once per (object, slot): a callback's result feeds the same slot back.
    if !self.typed_args_seen.insert((obj, slot)) {
      return;
    }
    let top = self.top_ext;
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
        self.admit_typed(obj, slot);
        self.typed_callback(obj, slot, back, site, mask);
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
            self.constrain(elem, Constraint::AdmitTyped(slot));
          }
        } else {
          self.admit_typed(obj, slot);
        }
        self.typed_nested_callbacks(obj, slot, back, site, mask);
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
  fn typed_callback(&mut self, f: ObjId, slot: VarId, back: VarId, site: Site, mask: u8) {
    if self.debug {
      self.callback_log.push((site.module.raw(), site.start, mask, slot, f));
    }
    if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_TRACKED_CALLBACKS").is_none() {
      if mask != 0 {
        self.constrain(slot, Constraint::Escape(Flavour::Ext, None));
      }
      self.escape(f, Flavour::Ext, None);
      // What it returns may come back out of the call wrapped in an Effect or a promise (a
      // `decode` succeeding with a service): what those succeed with goes to unseen code too.
      if let ObjKind::Closure(fid) = self.objs[f].kind {
        let ret = self.fns[fid].ret;
        self.constrain(ret, Constraint::EscapeOut);
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
        self.constrain(ret, Constraint::TypedArg(slot, back, site, 0xFF));
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
    self.constrain(callee, Constraint::Call(Box::new(call)));
    self.constrain(ret, Constraint::TypedArg(slot, back, site, 0xFF));
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
        self.constrain(this, Constraint::Escape(Flavour::Ext, None));
      }
      self.seed(call.ret, self.top_ext);
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
        self.typed_callback(obj, slot, back, site, mask);
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
        self.constrain(cb, Constraint::Call(Box::new(call)));
        self.wrap_callbacks.insert(w, cb);
        w
      }
    };
    self.holder(w)
  }

  pub fn top_of(&self, obj: ObjId) -> ObjId {
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
        self.seed(dst, self.top_ext);
      }
      ObjKind::Object | ObjKind::Array => {
        // A user object used as an effect (Effectable): what it yields is what its fields hold,
        // at worst. Effect's combinators take an `Effect`, whose type requires the
        // `~effect/Effect` member, so an object that can't have it is not run: it waits like any
        // value a declared type rejects (design section 8). Schedule's own code hands a
        // callback's result to `Effect.map` only once `isEffect` holds, for example.
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
  /// (issue, "Assumptions" 3), so the pass does not track failures, defects or Causes: handlers
  /// still run, and see only the `Done` signal that carries a Schedule's final output
  /// (`done_values`), as a Cause and as the error itself.
  fn failure_of(&mut self, obj: ObjId, dst: VarId) {
    if let ObjKind::Iterator(f) = self.objs[obj].kind {
      self.activate(f);
    }
    if matches!(
      self.objs[obj].kind,
      ObjKind::Effect { .. } | ObjKind::Iterator(_) | ObjKind::Wrap(_) | ObjKind::Typed { .. }
    ) {
      let (cause, done) = self.done_failure();
      self.seed(dst, cause);
      self.seed(dst, done);
    }
  }

  /// The Cause and `Done` of `done_failure`.
  pub fn done_failure(&mut self) -> (ObjId, ObjId) {
    if let Some(objs) = self.done_failure {
      return objs;
    }
    let done = self.alloc(ObjKind::Object, None);
    let tag = self.field(done, &ArcStr::from("_tag"));
    let s = self.str("Done");
    self.seed(tag, s);
    let brand = self.field(done, &ArcStr::from("~effect/Cause/Done"));
    self.seed(brand, self.prim);
    let value = self.field(done, &ArcStr::from("value"));
    self.copy(self.done_values, value);
    let reason = self.alloc(ObjKind::Object, None);
    let tag = self.field(reason, &ArcStr::from("_tag"));
    let s = self.str("Fail");
    self.seed(tag, s);
    let error = self.field(reason, &ArcStr::from("error"));
    self.seed(error, done);
    let reasons = self.alloc(ObjKind::Array, None);
    let elem = self.elem(reasons);
    self.seed(elem, reason);
    let cause = self.alloc(ObjKind::Object, None);
    let field = self.field(cause, &ArcStr::from("reasons"));
    self.seed(field, reasons);
    let brand = self.field(cause, &ArcStr::from("~effect/Cause"));
    self.seed(brand, self.prim);
    self.done_failure = Some((cause, done));
    (cause, done)
  }

  pub fn key_use(&mut self, obj: ObjId, use_: &KeyUse) {
    match self.objs[obj].kind.clone() {
      ObjKind::Key { strings, .. } => self.constrain(strings, Constraint::KeyUse(use_.clone())),
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
          self.provide_sources.insert(*src);
          let v = self.key_value_var(&name);
          self.copy(*src, v);
        }
        KeyUse::Strings(dst) => self.seed(*dst, obj),
      },
      ObjKind::AllKeys => match use_ {
        KeyUse::Lookup(dst) => self.copy(self.all_values, *dst),
        KeyUse::Provide(src) => {
          self.provide_sources.insert(*src);
          self.copy(*src, self.unknown_key_values);
        }
        KeyUse::Strings(dst) => self.seed(*dst, obj),
      },
      ObjKind::Object => {
        // A key-shaped literal (`{ key: "x" }`): its `key` field names the key.
        let field = self.field(obj, &ArcStr::from("key"));
        self.constrain(field, Constraint::KeyUse(use_.clone()));
        // Without a known `key` field the lookup is unknown.
        if self.vars[field].pts.is_empty() {
          self.key_use(self.all_keys, use_);
        }
      }
      ObjKind::Wrap(inner) => self.constrain(inner, Constraint::KeyUse(use_.clone())),
      // A type-modelled value used as a key: which key is unknown.
      ObjKind::Typed { .. } => self.key_use(self.all_keys, use_),
      _ => {
        // Anything else in a key position: an unknown key.
        let all = self.all_keys;
        self.key_use(all, use_);
      }
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
        self.constrain(strings, Constraint::KeyUse(use_.clone()));
        self.own_key_use(class, use_);
      }
      ObjKind::Class { heritage: Some(h), .. } => {
        self.constrain(h, Constraint::SubclassHeritage(use_.clone()));
      }
      ObjKind::Top(_) | ObjKind::External { .. } | ObjKind::Wrap(_) | ObjKind::Typed { .. } => {
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
      ObjKind::Key { .. } | ObjKind::Top(_) | ObjKind::External { .. } | ObjKind::Wrap(_) => true,
      ObjKind::Builtin { .. } => self.is_default(base),
      _ => false,
    };
    if possible_key {
      let all = self.all_keys;
      self.key_use(all, use_);
    }
  }

  /// A key class's own `key` field (`static key = "..."`) names its key.
  fn own_key_use(&mut self, class: ObjId, use_: &KeyUse) {
    let own = self.field(class, &ArcStr::from("key"));
    self.constrain(own, Constraint::KeyUse(use_.clone()));
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
      // of its keys. Reads on them are then recorded as demand, like any lookup.
      ObjKind::Context { keys } | ObjKind::Layer { keys } | ObjKind::Runtime { keys } => {
        self.constrain(keys, Constraint::KeyUse(KeyUse::Lookup(dst)));
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
    if self.admission
      && matches!(self.objs[obj].kind, ObjKind::Object | ObjKind::Array)
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
        let this = self.holder(obj);
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
    let method = self.var();
    self.constrain(this, Constraint::Load { key: Key::Name(ArcStr::from(name)), dst: method });
    let result = self.var();
    let call = Call {
      site: site.sub(method),
      this: Some(this),
      args: vec![Arg::Var(arg); arity],
      ret: result,
      is_new: false,
      builtin: None,
    };
    if self.admission {
      self.constrain(method, Constraint::ProtocolCall(Box::new(call)));
    } else {
      self.constrain(method, Constraint::Call(Box::new(call)));
    }
    self.copy(result, ret);
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
          self.recheck_tree(target);
        }
        if !self.objs[obj].spread_into.contains(&target) {
          self.objs[obj].spread_into.push(target);
        }
        // Loads already registered on the target must also see this source.
        let loads = self.objs[target].unknown_loads.clone();
        for dst in loads {
          self.load_unknown(obj, dst);
        }
        let named: Vec<(ArcStr, VarId)> =
          self.objs[target].fields.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (name, dst) in named {
          self.load_name(obj, &name, dst);
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

  pub fn bail_everything(&mut self, reason: &str) {
    self.bail_all = true;
    if self.bail_reasons.len() < 20 && !self.bail_reasons.iter().any(|r| r == reason) {
      self.bail_reasons.push(reason.to_string());
    }
  }

  /// A value handed to code the pass cannot see.
  pub fn escape(&mut self, obj: ObjId, flavour: Flavour, site: Option<Site>) {
    if self.objs[obj].escaped.is_some() {
      return;
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
      ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } => {
        if external {
          self.objs[obj].demand.all();
        }
        let fields: Vec<VarId> = self.objs[obj].fields.values().copied().collect();
        for f in fields {
          self.constrain(f, Constraint::Escape(flavour, site));
        }
        let elem = self.elem(obj);
        self.constrain(elem, Constraint::Escape(flavour, site));
        if external {
          // Fields written from outside may hold anything.
          self.seed(elem, self.top_ext);
          if !self.objs[obj].unknown_stores.contains(&elem) {
            self.objs[obj].unknown_stores.push(elem);
            self.recheck_tree(obj);
          }
        }
        let sources = self.objs[obj].spread_from.clone();
        for src in sources {
          self.constrain(src, Constraint::Escape(flavour, site));
        }
        if let ObjKind::Class { prototype, ctor, heritage } = self.objs[obj].kind {
          self.constrain(prototype, Constraint::Escape(flavour, site));
          if let Some(f) = ctor {
            self.escape_fn(f, flavour, site);
          }
          if let Some(h) = heritage {
            self.constrain(h, Constraint::Escape(flavour, site));
          }
        }
      }
      ObjKind::Closure(f) => self.escape_fn(f, flavour, site),
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

  fn escape_fn(&mut self, f: FnId, flavour: Flavour, site: Option<Site>) {
    self.activate(f);
    let (params, rest, this, ret) = {
      let i = &self.fns[f];
      (i.params.clone(), i.rest, i.this, i.ret)
    };
    let t = if flavour == Flavour::Ext { self.top_ext } else { self.top_host };
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
    self.constrain(ret, Constraint::Escape(flavour, site));
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
