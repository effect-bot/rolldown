//! Effect's own code.
//!
//! A call of an Effect API is evaluated through the API's definition in
//! Effect's modules, at the call site, with the call's arguments. The
//! evaluator interprets the JavaScript of those definitions: constants,
//! closures and their frames, objects and their fields, `this`, `new` with
//! prototypes, `arguments`, conditionals, `switch`, `try`. Calls of the
//! program's own functions go through the analysis above.
//!
//! Only what the fiber does is modelled, as the fiber interprets primitive
//! operations and never reads a Context itself: the primitives are recognised
//! by the `op` they are declared with (`makePrimitive`, `makeExit` and
//! `makePrimitiveProto` in `internal/core.js`), and the fiber methods a
//! continuation calls (`succeedWith`, `yieldWith`) return the Effect they are
//! given. A `withFiber` closure receives the fiber, whose `context` holds every
//! service.
//!
//! A definition the evaluator does not cover yields `None`, and the call falls
//! back to the treatment of an unanalysed API.

use std::rc::Rc;

use oxc::{
  allocator::Vec as ArenaVec,
  ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, AssignmentTarget, BinaryOperator,
    BindingPattern, CallExpression, ChainElement, Expression, FormalParameters, Function,
    ObjectPropertyKind, Program, Statement, UnaryOperator,
  },
  span::{GetSpan, Span},
};
use rolldown_common::{ModuleIdx, SymbolRef};
use rustc_hash::FxHashMap;

use super::{
  Arg, ArgKind, DeriveKey, FnId, Reach, ReachEvent, Resolved, Shared, Walker, key_name, strip,
};

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
  /// The key of a failure continuation (`contE`).
  cont_e_key: Option<String>,
  /// `F.prototype = P` for functions created inside Effect's code, by the
  /// function's parameter list.
  local_prototypes: std::cell::RefCell<FxHashMap<usize, Value<'s, 'a>>>,
  /// The superclass and instance of the constructor being run.
  supers: std::cell::RefCell<Option<(Option<Value<'s, 'a>>, Value<'s, 'a>)>>,
  /// The derived value of each call of an Effect API in the program, so an
  /// argument that is one keeps its structure.
  site_values: std::cell::RefCell<FxHashMap<(ModuleIdx, u32), Value<'s, 'a>>>,
  /// Functions of Effect's code being called, by parameter list, and the
  /// approximation of a recursive one's result.
  active: std::cell::RefCell<Vec<usize>>,
  recursive: std::cell::RefCell<FxHashMap<usize, (Value<'s, 'a>, bool)>>,
}

#[derive(Clone, Copy)]
enum Def<'s, 'a> {
  Expr(&'s Expression<'a>),
  Function(&'s Function<'a>),
  /// The fiber's own class (`FiberImpl` in `internal/effect.js`), which runs
  /// Effects: modelled, not interpreted.
  FiberClass,
  Class(&'s oxc::ast::ast::Class<'a>),
}

impl<'s, 'a> EffectDefs<'s, 'a> {
  pub(super) fn collect(
    &mut self,
    shared: &Shared<'_>,
    module: ModuleIdx,
    program: &'s Program<'a>,
  ) {
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
        Some(oxc::ast::ast::Declaration::ClassDeclaration(class)) => {
          let fiber = class.id.as_ref().is_some_and(|id| id.name == "FiberImpl")
            && shared.prefixes[module.index()].as_deref() == Some("internal/effect");
          if let Some(symbol) = class.id.as_ref().and_then(|id| id.symbol_id.get()) {
            let def = if fiber { Def::FiberClass } else { Def::Class(class) };
            self.defs.insert(canonical(symbol), (module, def));
          }
        }
        _ => {}
      }
    }
  }

  pub(super) fn find_keys(&mut self, shared: &Shared<'_>) {
    for (symbol, name) in &shared.api_names {
      match name.as_str() {
        "internal/core.args" => self.args_key = shared.const_strings.get(symbol).cloned(),
        "internal/core.contA" => self.cont_key = shared.const_strings.get(symbol).cloned(),
        "internal/core.contE" => self.cont_e_key = shared.const_strings.get(symbol).cloned(),
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
  /// A failure continuation, called with the input's cause.
  OnFailure,
  /// Both continuations.
  OnSuccessAndFailure,
  /// A generator driven by the fiber: succeeds with what it returns.
  Iterator,
  /// Registers a callback the fiber resumes with an Effect.
  Async,
  /// Succeeds with the input's Exit.
  Exit,
  /// `whileLoop({ while, body, step })`: runs `body` while `while()` holds,
  /// handing each success to `step`.
  While,
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
  /// Values stored under keys that are not constant: an array's elements, a
  /// Map's values, a Promise's resolution, a fiber's success.
  other: Value<'s, 'a>,
  primitive: Option<Primitive>,
  kind: ObjectKind,
  /// The class it is an instance of.
  class: Option<(ModuleIdx, &'s oxc::ast::ast::Class<'a>)>,
  /// Accessors by name: functions called with the object as `this`.
  getters: FxHashMap<String, Value<'s, 'a>>,
}

/// What builtin, or the fiber, an object is.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum ObjectKind {
  #[default]
  Plain,
  Array,
  Map,
  Promise,
  /// A fiber Effect's code created (`new FiberImpl`): `other` is what the
  /// Effects it runs succeed with.
  Fiber,
  /// An iterator over `other`.
  Iterator,
}

/// A builtin method, or a method of the fiber, bound to its receiver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
  /// `push`, `unshift`, `add`: stores its arguments.
  Store,
  /// `set(key, value)`: stores its second argument.
  StoreSecond,
  /// `pop`, `shift`, `at`, `get`, `find`: an element.
  Element,
  /// `forEach`, `map`, `filter`, ...: calls back with each element.
  CallBack,
  /// `slice`, `concat`, `values`, ...: a copy holding the elements.
  Copy,
  /// `join`, `toString`: converts the elements.
  Convert,
  /// `includes`, `has`, `delete`, `clear`, `indexOf`: Effect's own result.
  Opaque,
  /// `then(onFulfilled, onRejected)` of a Promise.
  Then,
  /// `catch(onRejected)` of a Promise.
  Catch,
  /// The fiber runs an Effect.
  Evaluate,
  /// The fiber calls back with its Exit when done.
  AddObserver,
  /// `[Symbol.iterator]()`: an iterator over the elements.
  Iterate,
  /// An iterator's `next()`.
  Next,
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
  /// A builtin method or a method of a created fiber, with its receiver.
  Method(Method, ObjectRef<'s, 'a>),
  /// The `resolve` of a Promise, or the `resume` of an `Async` primitive:
  /// stores its argument in the object.
  Resolve(ObjectRef<'s, 'a>),
  /// A Promise's `reject`: what it is given may be thrown at the program.
  Reject,
  /// A builtin constructor or function, by name (`Promise`, `Map`,
  /// `Object.create`).
  Builtin(&'static str),
  /// The fiber's class.
  FiberClass,
  /// A class of Effect's code.
  Class(ModuleIdx, &'s oxc::ast::ast::Class<'a>),
}

impl Callable<'_, '_> {
  /// The same function: joins keep one of each.
  fn same_as(&self, other: &Self) -> bool {
    match (self, other) {
      (Callable::Program(a), Callable::Program(b)) => a == b,
      (
        Callable::Closure { params: a, frame: fa, .. },
        Callable::Closure { params: b, frame: fb, .. },
      ) => std::ptr::eq(*a, *b) && Rc::ptr_eq(fa, fb),
      (Callable::Primitive(a), Callable::Primitive(b)) => a == b,
      (Callable::FiberMethod(a), Callable::FiberMethod(b)) => a == b,
      (Callable::Method(a, ta), Callable::Method(b, tb)) => a == b && Rc::ptr_eq(ta, tb),
      (Callable::Resolve(a), Callable::Resolve(b)) => Rc::ptr_eq(a, b),
      (Callable::Reject, Callable::Reject) | (Callable::FiberClass, Callable::FiberClass) => true,
      (Callable::Builtin(a), Callable::Builtin(b)) => a == b,
      (Callable::Class(ma, a), Callable::Class(mb, b)) => ma == mb && std::ptr::eq(*a, *b),
      _ => false,
    }
  }
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
#[expect(clippy::struct_excessive_bools, reason = "independent facts about one abstract value")]
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
  /// A generator object of the program's generator function.
  generator: Option<FnId>,
  /// A value of the program, data or a function, even one reaching nothing.
  program: bool,
  /// Spread into a call (`f(...list)`): stands for any number of arguments.
  spread: bool,
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
  /// A value that says nothing yet: the start of a join.
  fn is_empty(&self) -> bool {
    !self.reach.any()
      && self.callables.is_empty()
      && self.objects.is_empty()
      && self.constant.is_none()
      && !self.fiber
      && self.arguments.is_none()
      && self.primitive_proto.is_none()
      && !self.unknown
      && !self.opaque_callee
      && self.generator.is_none()
      && !self.program
      && !self.spread
  }
  /// Whether two values say the same, to detect a recursion's fixpoint.
  fn same_as(&self, other: &Value<'s, 'a>) -> bool {
    self.reach == other.reach
      && self.constant == other.constant
      && self.fiber == other.fiber
      && self.unknown == other.unknown
      && self.program == other.program
      && self.callables.len() == other.callables.len()
      && self.objects.len() == other.objects.len()
      && self.objects.iter().zip(&other.objects).all(|(a, b)| Rc::ptr_eq(a, b))
  }
  fn join(mut self, other: Value<'s, 'a>) -> Self {
    if self.is_empty() {
      return other;
    }
    if other.is_empty() {
      return self;
    }
    self.reach = self.reach.join(other.reach);
    self.program |= other.program;
    self.spread |= other.spread;
    for callable in other.callables {
      if !self.callables.iter().any(|known| known.same_as(&callable)) {
        self.callables.push(callable);
      }
    }
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
    match (self.generator, other.generator) {
      // Two generators in one value are not modelled.
      (Some(a), Some(b)) if a != b => self.opaque_callee = true,
      (None, generator) => self.generator = generator,
      _ => {}
    }
    self
  }
  fn object(object: Object<'s, 'a>) -> Self {
    Value { objects: vec![Rc::new(std::cell::RefCell::new(object))], ..Value::default() }
  }
  /// Whether the value may carry something of the program.
  fn carries_program(&self) -> bool {
    self.carries_program_within(&mut vec![], 0)
  }
  /// Whether the value may hold a runtime object: what a conversion or
  /// enumeration of it can observe.
  fn carries_runtime(&self) -> bool {
    self.carries_runtime_within(&mut vec![], 0)
  }
  fn carries_runtime_within(
    &self,
    seen: &mut Vec<*const std::cell::RefCell<Object<'s, 'a>>>,
    depth: usize,
  ) -> bool {
    if self.reach.any() || self.fiber || depth > DEFINITION_DEPTH {
      return true;
    }
    if self.arguments.as_ref().is_some_and(|arguments| {
      arguments.iter().any(|argument| argument.carries_runtime_within(seen, depth + 1))
    }) {
      return true;
    }
    self.objects.iter().any(|object| {
      if seen.contains(&Rc::as_ptr(object)) {
        return false;
      }
      seen.push(Rc::as_ptr(object));
      let object = object.borrow();
      object.kind == ObjectKind::Fiber
        || object.fields.values().any(|field| field.carries_runtime_within(seen, depth + 1))
        || object.other.carries_runtime_within(seen, depth + 1)
    })
  }
  /// Whether writing into the value may write into the program's values.
  fn carries_program_shallow_target(&self) -> bool {
    self.reach.any()
      || self.program
      || self.opaque_callee
      || self.callables.iter().any(|callable| matches!(callable, Callable::Program(_)))
  }
  fn carries_program_within(
    &self,
    seen: &mut Vec<*const std::cell::RefCell<Object<'s, 'a>>>,
    depth: usize,
  ) -> bool {
    if self.reach.any()
      || self.program
      || self.fiber
      || self.opaque_callee
      || self.generator.is_some()
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

const DEFINITION_DEPTH: usize = 64;
/// Expressions one derivation may evaluate.
const DERIVE_BUDGET: usize = 20_000;

impl<'s, 'a> Walker<'_, 's, '_, 'a> {
  /// The result of calling the Effect API bound to `api` with `args`, derived
  /// from its definition; `None` when the definition is outside what the
  /// evaluator covers.
  pub(super) fn derive_api(
    &mut self,
    api: SymbolRef,
    args: &[Arg],
    site: (ModuleIdx, u32),
  ) -> Option<Reach> {
    let key: DeriveKey =
      (api, args.iter().map(|arg| (arg.value.0, arg.functions.clone(), arg.site)).collect());
    if self.state.derive_failures.contains(&key) {
      return None;
    }
    if !self.state.final_walk {
      if let Some(cached) = self.state.derive_cache.get(&key) {
        return *cached;
      }
    }
    self.state.derive_stop = None;
    self.state.derive_budget = DERIVE_BUDGET;
    let outer = self.state.deriving.replace(api);
    let started = std::time::Instant::now();
    let result = self.derive_api_uncached(api, args, site);
    self.state.deriving = outer;
    #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
    if started.elapsed().as_millis() > 50
      && std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_DEBUG").is_some()
    {
      let name = self.shared().api_names.get(&api).map_or("?", String::as_str);
      eprintln!(
        "[reach] slow derivation {name}: {} ms, budget left {}, derived {}",
        started.elapsed().as_millis(),
        self.state.derive_budget,
        result.is_some()
      );
    }
    if result.is_none() {
      self.state.derive_failures.insert(key.clone());
      if let Some(stop) = self.state.derive_stop {
        self.state.derive_stops.entry(api).or_insert(stop);
      }
    }
    self.state.derive_cache.insert(key, result);
    result
  }

  fn derive_api_uncached(
    &mut self,
    api: SymbolRef,
    args: &[Arg],
    site: (ModuleIdx, u32),
  ) -> Option<Reach> {
    let mut values = vec![];
    for arg in args {
      // A spread, or a curried keyed lookup by a tracked key (followed per
      // key), is not followed into Effect's code. A value that may be a
      // function the analysis cannot see stops the derivation only if
      // Effect's code calls it.
      if arg.spread || arg.kind == ArgKind::Lookup {
        return None;
      }
      let derived = arg.site.and_then(|site| self.defs.site_values.borrow().get(&site).cloned());
      if let Some(value) = derived {
        values.push(value);
        continue;
      }
      values.push(Value {
        reach: arg.value,
        callables: arg.functions.iter().map(|function| Callable::Program(*function)).collect(),
        opaque_callee: matches!(arg.kind, ArgKind::Host | ArgKind::Unknown),
        program: true,
        ..Value::default()
      });
    }
    let callee = self.def_value(api, 0)?;
    let result = self.call_value_def(&callee, &Value::default(), &values, 0)?;
    if result.unknown {
      return None;
    }
    let reach = self.to_reach(&result, 0)?;
    #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
    if reach.value() && std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_TRACE").is_some() {
      let name = self.shared().api_names.get(&api).map_or("?", String::as_str);
      let args: Vec<_> = args.iter().map(|arg| (arg.value, arg.site.is_some())).collect();
      eprintln!(
        "[reach] {name} derived a runtime value from {args:?}: fiber={} objects={} reach={:?}",
        result.fiber,
        result.objects.len(),
        result.reach
      );
    }
    self.defs.site_values.borrow_mut().insert(site, result);
    let entry = self.state.derived_results.entry(api).or_default();
    *entry = entry.join(reach);
    Some(reach)
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
      Def::FiberClass => Some(Value { callables: vec![Callable::FiberClass], ..Value::default() }),
      Def::Class(class) => {
        Some(Value { callables: vec![Callable::Class(module, class)], ..Value::default() })
      }
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
      "OnFailure" => Primitive::OnFailure,
      "OnSuccessAndFailure" => Primitive::OnSuccessAndFailure,
      "Iterator" => Primitive::Iterator,
      "Async" => Primitive::Async,
      "Exit" => Primitive::Exit,
      "While" => Primitive::While,
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

  fn eval_def(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    expr: &'s Expression<'a>,
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    // A derivation that runs too long stops, like one the evaluator does not
    // cover.
    if self.state.derive_budget == 0 {
      return None;
    }
    self.state.derive_budget -= 1;
    let value = self.eval_def_inner(ctx, expr, depth);
    if value.is_none() && self.state.derive_stop.is_none() {
      self.state.derive_stop = Some((ctx.module, expr.span().start));
    }
    #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
    if value.is_none() && std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_TRACE").is_some() {
      let kind = format!("{expr:?}");
      eprintln!(
        "[reach] no value: module {:?} at {} ({})",
        ctx.module,
        expr.span().start,
        kind.split('(').next().unwrap_or_default()
      );
    }
    value
  }

  #[expect(clippy::too_many_lines, reason = "one arm per expression kind")]
  fn eval_def_inner(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    expr: &'s Expression<'a>,
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    if depth > DEFINITION_DEPTH {
      return None;
    }
    // Depth counts calls and definitions; nested expressions are bounded by
    // the syntax tree.
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
            if value.carries_runtime() {
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
        self.member_def(&object, Some(member.property.name.as_str()), member.span, depth)
      }
      Expression::ComputedMemberExpression(member) => {
        let object = self.eval_def(ctx, &member.object, depth)?;
        let key = self.eval_def(ctx, &member.expression, depth)?;
        let key = key.constant.as_ref().and_then(Constant::key);
        self.member_def(&object, key.as_deref(), member.span, depth)
      }
      Expression::ChainExpression(chain) => match &chain.expression {
        ChainElement::CallExpression(call) => return self.eval_def_call(ctx, call, depth),
        ChainElement::StaticMemberExpression(member) => {
          let object = self.eval_def(ctx, &member.object, depth)?;
          if member.optional
            && matches!(object.constant, Some(Constant::Undefined | Constant::Null))
          {
            // `undefined?.x` short-circuits.
            return Some(Value::constant(Constant::Undefined));
          }
          self.member_def(&object, Some(member.property.name.as_str()), member.span, depth)
        }
        ChainElement::ComputedMemberExpression(member) => {
          let object = self.eval_def(ctx, &member.object, depth)?;
          let key = self.eval_def(ctx, &member.expression, depth)?;
          let key = key.constant.as_ref().and_then(Constant::key);
          self.member_def(&object, key.as_deref(), member.span, depth)
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
      Expression::ClassExpression(class) => {
        Value { callables: vec![Callable::Class(ctx.module, class)], ..Value::default() }
      }
      Expression::PrivateFieldExpression(member) => {
        let object = self.eval_def(ctx, &member.object, depth)?;
        self.member_def(&object, Some(&format!("#{}", member.field.name)), member.span, depth)
      }
      Expression::ObjectExpression(object) => {
        let mut data = Object::default();
        for property in &object.properties {
          let property = match property {
            ObjectPropertyKind::SpreadProperty(spread) => {
              let value = self.eval_def(ctx, &spread.argument, depth)?;
              for source in &value.objects {
                let source = source.borrow();
                for (key, field) in &source.fields {
                  data.fields.insert(key.clone(), field.clone());
                }
                data.other = std::mem::take(&mut data.other).join(source.other.clone());
                if data.primitive.is_none() {
                  data.primitive = source.primitive;
                }
              }
              if value.reach.any()
                || value.callables.iter().any(|c| matches!(c, Callable::Program(_)))
              {
                data.other =
                  std::mem::take(&mut data.other).join(Value::reach(value.reach.member()));
              }
              continue;
            }
            ObjectPropertyKind::ObjectProperty(property) => property,
          };
          if property.kind != oxc::ast::ast::PropertyKind::Init {
            // An accessor (`get mapUnsafe()` of a Context): called on reads.
            if property.kind == oxc::ast::ast::PropertyKind::Get {
              let key = if property.computed {
                let key = self.eval_def(ctx, property.key.as_expression()?, depth)?;
                key.constant.as_ref().and_then(Constant::key)
              } else {
                key_name(&property.key)
              };
              let getter = self.eval_def(ctx, &property.value, depth)?;
              data.getters.insert(key?, getter);
            }
            continue;
          }
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
        let mut data = Object { kind: ObjectKind::Array, ..Object::default() };
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
        let nullish = matches!(left.constant, Some(Constant::Undefined | Constant::Null));
        match (logical.operator, decided) {
          (oxc::ast::ast::LogicalOperator::And, Some(true))
          | (oxc::ast::ast::LogicalOperator::Or, Some(false)) => right,
          (oxc::ast::ast::LogicalOperator::Coalesce, _) if nullish => right,
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
            if value.carries_runtime() {
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
        if assign.operator.is_logical() {
          // `a ??= b`: the binding keeps what it held, or gets `b`.
          self.assign_def(ctx, &assign.left, &value, depth)?;
          return Some(value.join(Value::unknown()));
        }
        if !assign.operator.is_assign() {
          return None;
        }
        self.assign_def(ctx, &assign.left, &value, depth)?;
        value
      }
      Expression::UpdateExpression(update) => {
        // `i++`: the binding or member now holds a number.
        let number = Value::unknown();
        match &update.argument {
          oxc::ast::ast::SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => {
            let scoping = self.shared().scoping(ctx.module);
            if let Some(symbol) = scoping.get_reference(ident.reference_id.get()?).symbol_id() {
              ctx.frame.assign((ctx.module, symbol), number.clone());
            }
          }
          oxc::ast::ast::SimpleAssignmentTarget::StaticMemberExpression(member) => {
            let object = self.eval_def(ctx, &member.object, depth)?;
            self.store_def(&object, Some(member.property.name.as_str()), &number)?;
          }
          oxc::ast::ast::SimpleAssignmentTarget::ComputedMemberExpression(member) => {
            let object = self.eval_def(ctx, &member.object, depth)?;
            let key = self.eval_def(ctx, &member.expression, depth)?;
            self.store_def(
              &object,
              key.constant.as_ref().and_then(Constant::key).as_deref(),
              &number,
            )?;
          }
          _ => return None,
        }
        number
      }
      Expression::CallExpression(call) => {
        if let Some(value) = self.primitive_declaration(ctx.module, expr) {
          return Some(value);
        }
        return self.eval_def_call(ctx, call, depth);
      }
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
    // An identifier an earlier optimisation rewrote has no reference: a
    // module-level binding by its name.
    let symbol = match ident.reference_id.get() {
      Some(reference) => scoping.get_reference(reference).symbol_id(),
      None => scoping.get_root_binding(ident.name.as_str().into()),
    };
    match symbol {
      Some(symbol) => {
        if let Some(value) = ctx.frame.get((ctx.module, symbol)) {
          return Some(value);
        }
        let canonical = self.shared().symbols.canonical_ref_for((ctx.module, symbol).into());
        if let Some(text) = self.shared().const_strings.get(&canonical) {
          return Some(Value::constant(Constant::Str(text.clone())));
        }
        if self.input.externals.contains(&canonical.owner) {
          // An import of a Node builtin: Effect's code calls it with its own
          // values.
          return Some(Value::unknown());
        }
        let value = self.def_value(canonical, depth);
        #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
        if value.is_none() && std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_TRACE").is_some() {
          eprintln!(
            "[reach] identifier `{}` (module {:?}) has no value; defined: {}",
            ident.name,
            canonical.owner,
            self.defs.defs.contains_key(&canonical)
          );
        }
        value
      }
      None => Some(match ident.name.as_str() {
        "undefined" => Value::constant(Constant::Undefined),
        "arguments" => Value { arguments: Some(Rc::clone(&ctx.arguments)), ..Value::default() },
        name @ ("Promise" | "Map" | "Set" | "WeakMap" | "WeakSet" | "Array" | "Object"
        | "Error" | "Symbol") => {
          let name: &'static str = match name {
            "Symbol" => "Symbol",
            "Promise" => "Promise",
            "Map" => "Map",
            "Set" => "Set",
            "WeakMap" => "WeakMap",
            "WeakSet" => "WeakSet",
            "Array" => "Array",
            "Object" => "Object",
            _ => "Error",
          };
          Value { callables: vec![Callable::Builtin(name)], ..Value::default() }
        }
        // The global object holds the running fiber.
        "globalThis" | "global" | "self" | "window" => Value { fiber: true, ..Value::default() },
        // Other builtins: usable to choose a branch, not to carry the
        // program's values.
        _ => Value::unknown(),
      }),
    }
  }

  /// `value.key`, or `value[unknown]` when `key` is `None`.
  fn member_def(
    &mut self,
    object: &Value<'s, 'a>,
    key: Option<&str>,
    span: Span,
    depth: usize,
  ) -> Value<'s, 'a> {
    let mut out = Value::default();
    let mut found = false;
    if object.fiber {
      found = true;
      match key {
        // The fiber's Context holds every service: runtime state, which
        // reaches anything once the program holds it. Its other members
        // are the fiber's operations.
        Some(name) if name != "context" => {
          out = out.join(Value {
            callables: vec![Callable::FiberMethod(name.to_string())],
            fiber: true,
            ..Value::default()
          });
        }
        _ => out = out.join(Value { fiber: true, ..Value::default() }),
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
    for target in &object.objects {
      found = true;
      let getter = key.and_then(|key| target.borrow().getters.get(key).cloned());
      if let Some(getter) = getter {
        let this = Value { objects: vec![Rc::clone(target)], ..Value::default() };
        out = out.join(self.call_value_def(&getter, &this, &[], depth + 1).unwrap_or_else(|| {
          #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
          if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_TRACE").is_some() {
            eprintln!("[reach] getter `{}` not evaluated", key.unwrap_or_default());
          }
          Value { fiber: true, ..Value::default() }
        }));
        continue;
      }
      let data = target.borrow();
      if let Some(method) =
        key.and_then(|key| Self::method_of(data.kind, key, data.fields.contains_key(key)))
      {
        out = out.join(Value {
          callables: vec![Callable::Method(method, Rc::clone(target))],
          ..Value::default()
        });
        continue;
      }
      if let (Some((module, class)), Some(key)) = (data.class, key) {
        if !data.fields.contains_key(key) {
          drop(data);
          if let Some((member, getter)) = self.class_member(module, class, key, false) {
            if getter {
              let this = Value { objects: vec![Rc::clone(target)], ..Value::default() };
              out = out.join(
                self
                  .call_value_def(&member, &this, &[], depth + 1)
                  .unwrap_or_else(|| Value { fiber: true, ..Value::default() }),
              );
            } else {
              out = out.join(member);
            }
            continue;
          }
          let data = target.borrow();
          out = out.join(data.other.clone());
          continue;
        }
      }
      let value = match (data.kind, key) {
        (ObjectKind::Fiber, Some("_exit")) => {
          self.exit_object(data.other.clone()).join(Value::constant(Constant::Undefined))
        }
        (ObjectKind::Fiber | ObjectKind::Promise, _)
        | (ObjectKind::Array | ObjectKind::Map, Some("length" | "size")) => Value::unknown(),
        (_, Some(key)) => {
          data.fields.get(key).cloned().unwrap_or_default().join(data.other.clone())
        }
        (_, None) => {
          data.fields.values().fold(data.other.clone(), |acc, value| acc.join(value.clone()))
        }
      };
      out = out.join(value);
    }
    for callable in &object.callables {
      if let (Callable::Builtin(name), Some(key)) = (callable, key) {
        found = true;
        let member: Option<&'static str> = match (*name, key) {
          ("Object", "create") => Some("Object.create"),
          ("Object", "assign") => Some("Object.assign"),
          ("Promise", "resolve") => Some("Promise.resolve"),
          ("Array", "from") => Some("Array.from"),
          ("Array", "isArray") => Some("Array.isArray"),
          ("Object", "values") => Some("Object.values"),
          ("Object", "entries") => Some("Object.entries"),
          ("Object", "keys") => Some("Object.keys"),
          _ => None,
        };
        if (*name, key) == ("Symbol", "iterator") {
          out = out.join(Value::constant(Constant::Str("@@iterator".to_string())));
          continue;
        }
        out = out.join(match member {
          Some(member) => Value { callables: vec![Callable::Builtin(member)], ..Value::default() },
          None => Value::unknown(),
        });
      }
      if let (Callable::Class(module, class), Some(key)) = (callable, key) {
        found = true;
        out = out.join(match self.class_member(*module, class, key, true) {
          Some((member, false)) => member,
          // A static getter, or a member the evaluator cannot read.
          _ => Value { fiber: true, ..Value::default() },
        });
        continue;
      }
      if matches!(
        callable,
        Callable::Closure { .. } | Callable::Primitive(_) | Callable::FiberClass
      ) {
        // A property of a function of Effect's code (`f.length`, `f.name`).
        found = true;
        out = out.join(Value::unknown());
      }
    }
    if (object.reach.any() || object.program)
      && matches!(key, Some("addObserver" | "_exit" | "removeObserver" | "interruptUnsafe"))
    {
      // The fiber's own interface, used by Effect's code on a fiber the
      // program holds: its Exit carries what the fiber succeeds with.
      found = true;
      let fiber = Rc::new(std::cell::RefCell::new(Object {
        kind: ObjectKind::Fiber,
        other: Value::reach(object.reach.fiber_success()),
        ..Object::default()
      }));
      out = out.join(match key {
        Some("_exit") => self
          .exit_object(Value::reach(object.reach.fiber_success()))
          .join(Value::constant(Constant::Undefined)),
        Some("addObserver") => Value {
          callables: vec![Callable::Method(Method::AddObserver, fiber)],
          ..Value::default()
        },
        _ => Value { callables: vec![Callable::Method(Method::Opaque, fiber)], ..Value::default() },
      });
    } else if (object.reach.any() || object.program || object.arguments.is_some())
      && key == Some("@@iterator")
    {
      // Iterating the program's value, or a list of arguments.
      found = true;
      let target = Rc::new(std::cell::RefCell::new(Object {
        kind: ObjectKind::Array,
        other: self.element_def(object),
        ..Object::default()
      }));
      out = out.join(Value {
        callables: vec![Callable::Method(Method::Iterate, target)],
        ..Value::default()
      });
    } else if object.reach.any()
      || object.program
      || object.callables.iter().any(|c| matches!(c, Callable::Program(_)))
    {
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
        program: true,
        ..Value::default()
      });
    }

    if object.constant.is_some() || object.unknown || !found {
      out = out.join(Value::unknown());
    }
    out
  }

  /// The builtin method `key` names on an object of this kind, unless the
  /// object has its own property by that name.
  #[expect(clippy::match_same_arms, reason = "a fiber's own methods come before its other members")]
  fn method_of(kind: ObjectKind, key: &str, own: bool) -> Option<Method> {
    if own {
      return None;
    }
    Some(match (kind, key) {
      (ObjectKind::Array | ObjectKind::Map, "push" | "unshift" | "add") => Method::Store,
      (ObjectKind::Map, "set") => Method::StoreSecond,
      (
        ObjectKind::Array | ObjectKind::Map,
        "pop" | "shift" | "at" | "get" | "find" | "findLast",
      ) => Method::Element,
      (
        ObjectKind::Array | ObjectKind::Map,
        "forEach" | "map" | "filter" | "flatMap" | "some" | "every" | "findIndex" | "reduce"
        | "reduceRight" | "sort",
      ) => Method::CallBack,
      (
        ObjectKind::Array | ObjectKind::Map,
        "slice" | "concat" | "values" | "keys" | "entries" | "reverse" | "splice" | "flat",
      ) => Method::Copy,
      (ObjectKind::Array | ObjectKind::Map, "join" | "toString" | "toLocaleString") => {
        Method::Convert
      }
      (
        ObjectKind::Array | ObjectKind::Map,
        "includes" | "indexOf" | "lastIndexOf" | "has" | "delete" | "clear" | "fill",
      ) => Method::Opaque,
      (ObjectKind::Array | ObjectKind::Map | ObjectKind::Iterator, "@@iterator") => Method::Iterate,
      (ObjectKind::Iterator, "next") => Method::Next,
      (ObjectKind::Promise, "then") => Method::Then,
      (ObjectKind::Promise, "catch") => Method::Catch,
      (ObjectKind::Fiber, "evaluate") => Method::Evaluate,
      (ObjectKind::Fiber, "addObserver") => Method::AddObserver,
      (ObjectKind::Fiber, _) => Method::Opaque,
      _ => return None,
    })
  }

  /// An Exit as the fiber reports it: succeeded with `value`, or failed with
  /// what may be thrown.
  fn exit_object(&self, value: Value<'s, 'a>) -> Value<'s, 'a> {
    let mut fields = FxHashMap::default();
    fields.insert("value".to_string(), value);
    fields.insert("cause".to_string(), self.cause_value());
    fields.insert("_tag".to_string(), Value::unknown());
    Value::object(Object { fields, primitive: Some(Primitive::Success), ..Object::default() })
  }

  /// A failure's cause as Effect builds it: reasons holding what may be
  /// thrown.
  fn cause_value(&self) -> Value<'s, 'a> {
    let thrown = Value { reach: self.state.thrown.collapse(), program: true, ..Value::default() };
    let mut reason = Object::default();
    reason.fields.insert("error".to_string(), thrown.clone());
    reason.fields.insert("defect".to_string(), thrown);
    reason.fields.insert("_tag".to_string(), Value::unknown());
    reason.fields.insert("annotations".to_string(), Value::unknown());
    let reasons =
      Object { kind: ObjectKind::Array, other: Value::object(reason), ..Object::default() };
    let mut cause = Object::default();
    cause.fields.insert("reasons".to_string(), Value::object(reasons));
    cause.fields.insert("_tag".to_string(), Value::unknown());
    Value::object(cause)
  }

  /// An element of an iterable value.
  fn element_def(&self, value: &Value<'s, 'a>) -> Value<'s, 'a> {
    let mut out = Value {
      reach: value.reach.element(),
      program: value.program,
      callables: value
        .callables
        .iter()
        .filter(|c| matches!(c, Callable::Program(_)))
        .cloned()
        .collect(),
      ..Value::default()
    };
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
        if left.carries_runtime() || right.carries_runtime() {
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
        if member.property.name == "prototype" {
          // `F.prototype = P` for a function Effect's code created.
          let mut recorded = false;
          for callable in &object.callables {
            if let Callable::Closure { params, .. } = callable {
              self
                .defs
                .local_prototypes
                .borrow_mut()
                .insert(std::ptr::from_ref(*params).addr(), value.clone());
              recorded = true;
            }
          }
          if recorded {
            return Some(());
          }
        }
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
            _ => {
              // A list of unknown length: any number of arguments.
              let mut element = self.element_def(&value);
              element.spread = true;
              out.push(element);
            }
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
    if matches!(call.callee, Expression::Super(_)) {
      let args = self.eval_def_args(ctx, &call.arguments, depth)?;
      let (parent, this) = self.defs.supers.borrow().clone()?;
      self.construct_parent(parent.as_ref(), &this, &args, depth)?;
      return Some(Value::default());
    }
    // `x?.f()` and `x.y?.f()` with `undefined` short-circuit.
    if let Expression::StaticMemberExpression(member) = strip(&call.callee) {
      if member.optional {
        let object = self.eval_def(ctx, &member.object, depth)?;
        if matches!(object.constant, Some(Constant::Undefined | Constant::Null)) {
          return Some(Value::constant(Constant::Undefined));
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
        (self.member_def(&object, Some(member.property.name.as_str()), member.span, depth), object)
      }
      Expression::ComputedMemberExpression(member) => {
        let object = self.eval_def(ctx, &member.object, depth)?;
        let key = self.eval_def(ctx, &member.expression, depth)?;
        let key = key.constant.as_ref().and_then(Constant::key);
        (self.member_def(&object, key.as_deref(), member.span, depth), object)
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
    if let [Callable::Builtin(name)] = callee.callables.as_slice() {
      return self.construct_builtin(name, &args, depth);
    }
    if let [Callable::FiberClass] = callee.callables.as_slice() {
      // `new FiberImpl(context, ...)`: a fiber that has run nothing yet.
      return Some(Value::object(Object { kind: ObjectKind::Fiber, ..Object::default() }));
    }
    if let [Callable::Class(module, class)] = callee.callables.as_slice() {
      let object = Rc::new(std::cell::RefCell::new(Object {
        class: Some((*module, *class)),
        ..Object::default()
      }));
      let this = Value { objects: vec![object], ..Value::default() };
      self.construct_class(*module, class, &this, &args, depth)?;
      return Some(this);
    }
    let builtin_like = callee.callables.iter().all(|c| matches!(c, Callable::FiberMethod(_)));
    if builtin_like {
      // A builtin constructor (`Error`, `globalThis.Error`, an import of a
      // Node builtin): it may only see Effect's own values.
      if (callee.unknown || callee.fiber) && !args.iter().any(Value::carries_program) {
        return Some(Value::unknown());
      }
      return None;
    }
    let mut out = Value::default();
    for callable in callee.callables.clone() {
      let Callable::Closure { symbol, params, .. } = &callable else { return None };
      let local =
        self.defs.local_prototypes.borrow().get(&std::ptr::from_ref(*params).addr()).cloned();
      let primitive =
        match (local, symbol.and_then(|symbol| self.defs.prototypes.get(&symbol).copied())) {
          (Some(proto), _) => proto.primitive_proto,
          (None, Some((module, proto))) => {
            let ctx = Ctx {
              module,
              frame: Rc::default(),
              this: Value::default(),
              arguments: Rc::default(),
            };
            self.eval_def(&ctx, proto, depth)?.primitive_proto
          }
          (None, None) => None,
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
    #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
    if callee.callables.is_empty()
      && !(callee.unknown && !args.iter().any(Value::carries_program) && !this.carries_program())
      && std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_TRACE").is_some()
    {
      eprintln!(
        "[reach] not callable: reach={:?} program={} unknown={} fiber={} objects={} constant={:?} args_program={} this_program={}",
        callee.reach,
        callee.program,
        callee.unknown,
        callee.fiber,
        callee.objects.len(),
        callee.constant,
        args.iter().any(Value::carries_program),
        this.carries_program()
      );
    }
    if callee.callables.is_empty() && (callee.fiber || this.fiber || this.reach.value()) {
      #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
      if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_TRACE").is_some() {
        let name = self
          .state
          .deriving
          .and_then(|api| self.shared().api_names.get(&api))
          .map_or("?", String::as_str);
        eprintln!(
          "[reach] runtime method in {name}: callee fiber={} this fiber={} this reach={:?} this program={}",
          callee.fiber, this.fiber, this.reach, this.program
        );
      }
      // A method of a runtime object (a Context, a memo map, a fiber): its
      // result is runtime state, and functions it is given may be called
      // back with that state.
      let state = Value { fiber: true, ..Value::default() };
      self.call_back_unknown(args, &state, depth)?;
      return Some(state);
    }
    if callee.callables.is_empty() {
      // A builtin or an unknown function, called with Effect's own values
      // only: its result can choose a branch. Functions it is given may be
      // called back with its own values.
      if callee.unknown && !args.iter().any(Value::carries_program) && !this.carries_program() {
        self.call_back_unknown(args, &Value::unknown(), depth)?;
        return Some(Value::unknown());
      }
      return None;
    }
    let mut result = Value::default();
    for callable in callee.callables.clone() {
      let value = match callable {
        Callable::Program(function) => self.call_program_from_def(function, args)?,
        Callable::Closure { module, params, body, frame, lexical, .. } => {
          let key = std::ptr::from_ref(params).addr();
          if self.defs.active.borrow().contains(&key) {
            // Recursion: the current approximation of the result.
            let mut recursive = self.defs.recursive.borrow_mut();
            let entry = recursive.entry(key).or_default();
            entry.1 = true;
            result = result.join(entry.0.clone());
            continue;
          }
          self.defs.active.borrow_mut().push(key);
          let mut value = None;
          for _ in 0..3 {
            self.defs.recursive.borrow_mut().entry(key).or_default().1 = false;
            let next =
              self.call_closure(module, params, body, &frame, lexical.clone(), this, args, depth);
            let Some(next) = next else {
              value = None;
              break;
            };
            let mut recursive = self.defs.recursive.borrow_mut();
            let entry = recursive.entry(key).or_default();
            let again = entry.1 && !entry.0.same_as(&next);
            entry.0 = std::mem::take(&mut entry.0).join(next.clone());
            value = Some(next);
            if !again {
              break;
            }
          }
          self.defs.active.borrow_mut().pop();
          let value = value?;
          result = result.join(value);
          continue;
        }
        Callable::Primitive(primitive) => self.primitive(primitive, args, depth)?,
        Callable::Method(method, target) => self.call_method_def(method, &target, args, depth)?,
        Callable::Resolve(target) => {
          let value = args.first().cloned().unwrap_or_default();
          // Resolving with a Promise adopts its resolution.
          let value = Self::settled(&value);
          let mut data = target.borrow_mut();
          data.other = std::mem::take(&mut data.other).join(value);
          Value::constant(Constant::Undefined)
        }
        Callable::Reject => {
          let thrown = match args.first() {
            Some(value) => self.thrown_reach_exact(value, depth)?,
            None => Reach::NONE,
          };
          self.join_thrown(thrown, "Promise reject in Effect code");
          Value::constant(Constant::Undefined)
        }
        Callable::Builtin(name) => self.call_builtin_def(name, args, depth)?,
        // Classes are constructed with `new`, not called.
        Callable::FiberClass | Callable::Class(..) => return None,
        Callable::FiberMethod(name) => match name.as_str() {
          // The current Effect succeeds with the value, or with the Exit.
          "succeedWith" => self.exit_object(args.first()?.clone()),
          "yieldWith" => args.first()?.clone(),
          // The running fiber's other operations: what they store stays in
          // the fiber, whose reads already reach every service, and what
          // they return is the fiber's own state, which may hold runtime
          // objects (`children()`), so it reaches anything. Functions they
          // are given may be called back with that state.
          _ => {
            let state = Value { fiber: true, ..Value::default() };
            self.call_back_unknown(args, &state, depth)?;
            state
          }
        },
      };
      result = result.join(value);
    }
    Some(result)
  }

  /// What awaiting a value gives: a Promise's resolution, or the value.
  fn settled(value: &Value<'s, 'a>) -> Value<'s, 'a> {
    let mut out = Value { objects: vec![], ..value.clone() };
    for object in &value.objects {
      let data = object.borrow();
      if data.kind == ObjectKind::Promise {
        out = out.join(data.other.clone());
      } else {
        out.objects.push(Rc::clone(object));
      }
    }
    out
  }

  /// Calls the functions among `args` with `with` for every parameter, as
  /// code the evaluator does not interpret may.
  fn call_back_unknown(
    &mut self,
    args: &[Value<'s, 'a>],
    with: &Value<'s, 'a>,
    depth: usize,
  ) -> Option<()> {
    for arg in args {
      if arg.callables.is_empty() {
        continue;
      }
      let callable = Value { callables: arg.callables.clone(), ..Value::default() };
      self.call_value_def(
        &callable,
        with,
        &[with.clone(), with.clone(), with.clone()],
        depth + 1,
      )?;
    }
    Some(())
  }

  /// Calls a builtin method, or a method of a created fiber.
  fn call_method_def(
    &mut self,
    method: Method,
    target: &ObjectRef<'s, 'a>,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    let elements = target.borrow().other.clone();
    let this = Value { objects: vec![Rc::clone(target)], ..Value::default() };
    Some(match method {
      Method::Store => {
        let mut data = target.borrow_mut();
        for arg in args {
          data.other = std::mem::take(&mut data.other).join(arg.clone());
        }
        Value::unknown()
      }
      Method::StoreSecond => {
        if let Some(value) = args.get(1) {
          let mut data = target.borrow_mut();
          data.other = std::mem::take(&mut data.other).join(value.clone());
        }
        this
      }
      Method::Element => elements.join(Value::constant(Constant::Undefined)),
      Method::CallBack => {
        let callback = args.first().cloned().unwrap_or_default();
        let ret = self.call_value_def(
          &callback,
          &Value::default(),
          &[elements.clone(), Value::unknown(), this],
          depth + 1,
        )?;
        let mut data = Object { kind: ObjectKind::Array, ..Object::default() };
        data.other = elements.join(ret.clone());
        Value::object(data).join(ret)
      }
      Method::Copy => {
        let mut data = Object { kind: ObjectKind::Array, ..Object::default() };
        data.other = args.iter().fold(elements, |acc, arg| acc.join(self.element_def(arg)));
        Value::object(data)
      }
      Method::Convert => {
        if elements.carries_runtime() {
          self.observe_all(Reach::TOP, Span::default(), "converted to a string in Effect's code");
        }
        Value::unknown()
      }
      Method::Opaque => {
        if args.iter().any(Value::carries_program) {
          return None;
        }
        Value::unknown()
      }
      Method::Then | Method::Catch => {
        let resolution = elements;
        let (on_fulfilled, on_rejected) = match method {
          Method::Then => (args.first().cloned(), args.get(1).cloned()),
          _ => (None, args.first().cloned()),
        };
        let mut settled = Value::default();
        match on_fulfilled {
          Some(callback) if !callback.callables.is_empty() || callback.opaque_callee => {
            let ret =
              self.call_value_def(&callback, &Value::default(), &[resolution], depth + 1)?;
            settled = settled.join(Self::settled(&ret));
          }
          _ => settled = settled.join(resolution),
        }
        if let Some(callback) = on_rejected.filter(|c| !c.callables.is_empty() || c.opaque_callee) {
          let cause = self.cause_value();
          let ret = self.call_value_def(&callback, &Value::default(), &[cause], depth + 1)?;
          settled = settled.join(Self::settled(&ret));
        }
        Value::object(Object { kind: ObjectKind::Promise, other: settled, ..Object::default() })
      }
      Method::Iterate => {
        Value::object(Object { kind: ObjectKind::Iterator, other: elements, ..Object::default() })
      }
      Method::Next => {
        let mut fields = FxHashMap::default();
        fields.insert("value".to_string(), elements.join(Value::constant(Constant::Undefined)));
        fields.insert("done".to_string(), Value::unknown());
        Value::object(Object { fields, ..Object::default() })
      }
      Method::Evaluate => {
        let effect = args.first().cloned().unwrap_or_default();
        let success = self.to_reach(&effect, depth + 1)?.success();
        let mut data = target.borrow_mut();
        data.other = std::mem::take(&mut data.other).join(Value::reach(success));
        Value::unknown()
      }
      Method::AddObserver => {
        let observer = args.first().cloned().unwrap_or_default();
        let exit = self.exit_object(elements);
        self.call_value_def(&observer, &Value::default(), &[exit], depth + 1)?;
        Value::unknown()
      }
    })
  }

  /// Calls a builtin function (`Object.create`, `Promise.resolve`, ...).
  fn call_builtin_def(
    &mut self,
    name: &'static str,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    let first = args.first().cloned().unwrap_or_default();
    Some(match name {
      "Object.create" => {
        // The prototype's members are read through the new object.
        let mut data = Object { primitive: first.primitive_proto, ..Object::default() };
        for proto in &first.objects {
          let proto = proto.borrow();
          data.fields.clone_from(&proto.fields);
          data.other = proto.other.clone();
          data.primitive = data.primitive.or(proto.primitive);
        }
        Value::object(data)
      }
      "Promise.resolve" => Value::object(Object {
        kind: ObjectKind::Promise,
        other: Self::settled(&first),
        ..Object::default()
      }),
      "Object.values" | "Object.entries" | "Object.keys" => {
        if first.reach.value() {
          self.observe_all(Reach::TOP, Span::default(), "enumerated in Effect's code");
        }
        let mut data = Object { kind: ObjectKind::Array, ..Object::default() };
        let values = self.member_def(&first, None, Span::default(), depth);
        data.other = match name {
          "Object.keys" => Value::unknown(),
          "Object.values" => values,
          _ => {
            let mut entry = Object { kind: ObjectKind::Array, ..Object::default() };
            entry.other = values.join(Value::unknown());
            Value::object(entry)
          }
        };
        Value::object(data)
      }
      "Array.from" => {
        let mut data = Object { kind: ObjectKind::Array, ..Object::default() };
        data.other = self.element_def(&first);
        if let Some(map) = args.get(1) {
          let ret =
            self.call_value_def(map, &Value::default(), &[data.other.clone()], depth + 1)?;
          data.other = ret;
        }
        Value::object(data)
      }
      _ => {
        if args.iter().any(Value::carries_program) {
          return None;
        }
        Value::unknown()
      }
    })
  }

  /// `new` of a builtin constructor.
  fn construct_builtin(
    &mut self,
    name: &'static str,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    Some(match name {
      "Promise" => {
        let target = Rc::new(std::cell::RefCell::new(Object {
          kind: ObjectKind::Promise,
          ..Object::default()
        }));
        let executor = args.first().cloned().unwrap_or_default();
        let resolve =
          Value { callables: vec![Callable::Resolve(Rc::clone(&target))], ..Value::default() };
        let reject = Value { callables: vec![Callable::Reject], ..Value::default() };
        self.call_value_def(&executor, &Value::default(), &[resolve, reject], depth + 1)?;
        Value { objects: vec![target], ..Value::default() }
      }
      "Map" | "Set" | "WeakMap" | "WeakSet" | "Array" => {
        let mut data = Object {
          kind: if matches!(name, "Array") { ObjectKind::Array } else { ObjectKind::Map },
          ..Object::default()
        };
        for arg in args {
          data.other = std::mem::take(&mut data.other).join(self.element_def(arg));
        }
        Value::object(data)
      }
      _ => {
        if args.iter().any(Value::carries_program) {
          return None;
        }
        Value::unknown()
      }
    })
  }

  /// Calls a function of Effect's code once.
  #[expect(clippy::too_many_arguments, reason = "the parts of a closure and its call")]
  fn call_closure(
    &mut self,
    module: ModuleIdx,
    params: &'s FormalParameters<'a>,
    body: ClosureBody<'s, 'a>,
    frame: &Rc<Frame<'s, 'a>>,
    lexical: Option<(Box<Value<'s, 'a>>, Rc<[Value<'s, 'a>]>)>,
    this: &Value<'s, 'a>,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<Value<'s, 'a>> {
    let frame = Frame::child(frame);
    // A spread of unknown length: any argument may land in any parameter.
    let spread = args.iter().any(|arg| arg.spread);
    let all = args
      .iter()
      .fold(Value::default(), |acc, arg| acc.join(Value { spread: false, ..arg.clone() }));
    let (this, arguments): (Value<'s, 'a>, Rc<[Value<'s, 'a>]>) = match lexical {
      Some((this, arguments)) => (*this, arguments),
      None if spread => (this.clone(), Rc::from(vec![all.clone()])),
      None => (this.clone(), Rc::from(args.to_vec())),
    };
    let ctx = Ctx { module, frame, this, arguments };
    for (index, param) in params.items.iter().enumerate() {
      let mut value = if spread {
        all.clone().join(Value::constant(Constant::Undefined))
      } else {
        args.get(index).cloned().unwrap_or_else(|| Value::constant(Constant::Undefined))
      };
      if let Some(initializer) = &param.initializer {
        value = value.join(self.eval_def(&ctx, initializer, depth + 1)?);
      }
      self.bind_def(&ctx, &param.pattern, value, depth + 1)?;
    }
    if let Some(rest) = &params.rest {
      let value = if spread {
        // A list of unknown length.
        Value::object(Object { kind: ObjectKind::Array, other: all, ..Object::default() })
      } else {
        // The rest of the arguments, as a list of known length.
        let rest_args: Vec<Value<'s, 'a>> = args.iter().skip(params.items.len()).cloned().collect();
        Value { arguments: Some(Rc::from(rest_args)), ..Value::default() }
      };
      self.bind_def(&ctx, &rest.rest.argument, value, depth + 1)?;
    }
    Some(match body {
      ClosureBody::Expr(expression) => self.eval_def(&ctx, expression, depth + 1)?,
      ClosureBody::Block(body) => {
        let flow = self.exec_def(&ctx, &body.statements, depth + 1)?;
        if flow.falls_through {
          flow.returns.join(Value::constant(Constant::Undefined))
        } else {
          flow.returns
        }
      }
    })
  }

  /// A method, getter or field of a class, or of its superclasses: a
  /// static one when `is_static`.
  fn class_member(
    &mut self,
    module: ModuleIdx,
    class: &'s oxc::ast::ast::Class<'a>,
    key: &str,
    is_static: bool,
  ) -> Option<(Value<'s, 'a>, bool)> {
    for element in &class.body.body {
      match element {
        oxc::ast::ast::ClassElement::MethodDefinition(method)
          if method.r#static == is_static
            && method.kind != oxc::ast::ast::MethodDefinitionKind::Constructor
            && Self::element_key(&method.key).as_deref() == Some(key) =>
        {
          if method.kind == oxc::ast::ast::MethodDefinitionKind::Set {
            continue;
          }
          let body = method.value.body.as_deref()?;
          let getter = method.kind == oxc::ast::ast::MethodDefinitionKind::Get;
          return Some((
            Value {
              callables: vec![Callable::Closure {
                module,
                params: &method.value.params,
                body: ClosureBody::Block(body),
                frame: Rc::default(),
                symbol: None,
                lexical: None,
              }],
              ..Value::default()
            },
            getter,
          ));
        }
        oxc::ast::ast::ClassElement::PropertyDefinition(property)
          if is_static
            && property.r#static
            && Self::element_key(&property.key).as_deref() == Some(key) =>
        {
          let ctx =
            Ctx { module, frame: Rc::default(), this: Value::default(), arguments: Rc::default() };
          return match &property.value {
            Some(value) => self.eval_def(&ctx, value, 0).map(|value| (value, false)),
            None => Some((Value::constant(Constant::Undefined), false)),
          };
        }
        _ => {}
      }
    }
    // The superclass.
    let heritage = class.heritage.as_ref()?;
    let ctx =
      Ctx { module, frame: Rc::default(), this: Value::default(), arguments: Rc::default() };
    let parent = self.eval_def(&ctx, &heritage.expression, 0)?;
    match parent.callables.as_slice() {
      [Callable::Class(module, parent)] => self.class_member(*module, parent, key, is_static),
      _ => None,
    }
  }

  /// The instance getters of a class and its superclasses.
  fn class_getters(
    &mut self,
    module: ModuleIdx,
    class: &'s oxc::ast::ast::Class<'a>,
  ) -> Vec<Value<'s, 'a>> {
    let mut out = vec![];
    for element in &class.body.body {
      if let oxc::ast::ast::ClassElement::MethodDefinition(method) = element {
        if method.kind == oxc::ast::ast::MethodDefinitionKind::Get && !method.r#static {
          if let Some(body) = method.value.body.as_deref() {
            out.push(Value {
              callables: vec![Callable::Closure {
                module,
                params: &method.value.params,
                body: ClosureBody::Block(body),
                frame: Rc::default(),
                symbol: None,
                lexical: None,
              }],
              ..Value::default()
            });
          }
        }
      }
    }
    if let Some(heritage) = &class.heritage {
      let ctx =
        Ctx { module, frame: Rc::default(), this: Value::default(), arguments: Rc::default() };
      if let Some(parent) = self.eval_def(&ctx, &heritage.expression, 0) {
        if let [Callable::Class(module, parent)] = parent.callables.as_slice() {
          out.extend(self.class_getters(*module, parent));
        }
      }
    }
    out
  }

  fn element_key(key: &oxc::ast::ast::PropertyKey<'_>) -> Option<String> {
    match key {
      oxc::ast::ast::PropertyKey::PrivateIdentifier(private) => Some(format!("#{}", private.name)),
      key => key_name(key),
    }
  }

  /// Runs a class's field initialisers and constructor on `this`.
  fn construct_class(
    &mut self,
    module: ModuleIdx,
    class: &'s oxc::ast::ast::Class<'a>,
    this: &Value<'s, 'a>,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<()> {
    if depth > DEFINITION_DEPTH {
      return None;
    }
    let ctx =
      Ctx { module, frame: Rc::default(), this: this.clone(), arguments: Rc::from(args.to_vec()) };
    let parent = match &class.heritage {
      Some(heritage) => Some(self.eval_def(&ctx, &heritage.expression, depth + 1)?),
      None => None,
    };
    let constructor = class.body.body.iter().find_map(|element| match element {
      oxc::ast::ast::ClassElement::MethodDefinition(method)
        if method.kind == oxc::ast::ast::MethodDefinitionKind::Constructor =>
      {
        Some(method)
      }
      _ => None,
    });
    if constructor.is_none() {
      // An implicit constructor passes its arguments to the superclass.
      self.construct_parent(parent.as_ref(), this, args, depth)?;
    }
    for element in &class.body.body {
      if let oxc::ast::ast::ClassElement::PropertyDefinition(property) = element {
        if property.r#static {
          continue;
        }
        let key = Self::element_key(&property.key);
        let value = match &property.value {
          Some(value) => self.eval_def(&ctx, value, depth + 1)?,
          None => Value::constant(Constant::Undefined),
        };
        self.store_def(this, key.as_deref(), &value)?;
      }
    }
    if let Some(constructor) = constructor {
      let body = constructor.value.body.as_deref()?;
      let frame: Rc<Frame<'s, 'a>> = Rc::default();
      let callable = Value {
        callables: vec![Callable::Closure {
          module,
          params: &constructor.value.params,
          body: ClosureBody::Block(body),
          frame: Rc::clone(&frame),
          symbol: None,
          lexical: None,
        }],
        ..Value::default()
      };
      // `super(...)` inside the constructor reaches the superclass.
      let previous = self.defs.supers.borrow_mut().replace((parent.clone(), this.clone()));
      let ran = self.call_value_def(&callable, this, args, depth + 1);
      *self.defs.supers.borrow_mut() = previous;
      ran?;
    }
    Some(())
  }

  fn construct_parent(
    &mut self,
    parent: Option<&Value<'s, 'a>>,
    this: &Value<'s, 'a>,
    args: &[Value<'s, 'a>],
    depth: usize,
  ) -> Option<()> {
    let Some(parent) = parent else { return Some(()) };
    match parent.callables.as_slice() {
      [Callable::Class(module, class)] => {
        self.construct_class(*module, class, this, args, depth + 1)
      }
      // A builtin superclass (`Error`): it stores what it is given.
      _ => {
        for arg in args {
          self.store_def(this, None, arg)?;
        }
        Some(())
      }
    }
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
      program_args.push(Arg { value, spread: false, functions, kind: ArgKind::Known, site: None });
    }
    let effect_fn = self.info.effect_generators.contains(&function);
    let ret = self.call_function(function, effect_fn, &program_args, Reach::NONE);
    if self.info.generator_fns.contains(&function) {
      // A generator object: the `Iterator` primitive reads what it returns.
      return Some(Value { generator: Some(function), ..Value::default() });
    }
    // Functions the program returns are not followed back into Effect's code.
    Some(Value::reach(ret))
  }

  /// What a value of Effect's code means to the program: its reach, with
  /// Effects built from primitives read as the success they run to.
  fn to_reach(&mut self, value: &Value<'s, 'a>, depth: usize) -> Option<Reach> {
    self.to_reach_seen(value, depth)
  }

  fn to_reach_seen(&mut self, value: &Value<'s, 'a>, depth: usize) -> Option<Reach> {
    // What a continuation returns is a new computation: a fresh `seen`.
    self.to_reach_within(value, depth, &mut vec![])
  }

  fn to_reach_within(
    &mut self,
    value: &Value<'s, 'a>,
    depth: usize,
    seen: &mut Vec<*const std::cell::RefCell<Object<'s, 'a>>>,
  ) -> Option<Reach> {
    if depth > DEFINITION_DEPTH || value.opaque_callee || self.state.derive_budget == 0 {
      return None;
    }
    self.state.derive_budget -= 1;
    let mut reach = value.reach;
    if value.fiber {
      reach = Reach::TOP;
    }
    if let Some(arguments) = &value.arguments {
      for argument in arguments.iter() {
        reach = reach.join(self.to_reach_within(argument, depth + 1, seen)?);
      }
    }
    if let Some(generator) = value.generator {
      reach = reach.join(self.state.ret(generator).collapse());
    }
    for object in &value.objects {
      // An object already being read contributes through that read.
      if seen.contains(&Rc::as_ptr(object)) {
        continue;
      }
      seen.push(Rc::as_ptr(object));
      let (primitive, kind, fields, other) = {
        let object = object.borrow();
        (object.primitive, object.kind, object.fields.clone(), object.other.clone())
      };
      match kind {
        ObjectKind::Promise => {
          reach = reach.join(self.to_reach_within(&other, depth + 1, seen)?.effect_of());
          continue;
        }
        ObjectKind::Fiber => {
          reach = reach.join(self.to_reach_within(&other, depth + 1, seen)?.effect_of().fiber_of());
          continue;
        }
        ObjectKind::Plain | ObjectKind::Array | ObjectKind::Map | ObjectKind::Iterator => {}
      }
      let fiber = Value { fiber: true, ..Value::default() };
      let cause = self.cause_value();
      let field = |key: &Option<String>| key.as_ref().and_then(|key| fields.get(key)).cloned();
      match primitive {
        Some(Primitive::Success) => {
          let value = fields.get("value").cloned().unwrap_or_default();
          reach = reach.join(self.to_reach_within(&value, depth + 1, seen)?.effect_of());
          continue;
        }
        Some(Primitive::Failure) => continue,
        Some(Primitive::Exit) => {
          let input = field(&self.defs.args_key)?;
          reach = reach.join(self.to_reach_seen(&input, depth + 1)?.success().effect_of());
          continue;
        }
        Some(Primitive::Iterator) => {
          // The program's generator, driven by the fiber: succeeds with what
          // it returns; `yield*` inside it is analysed as the program's code.
          let iterator = fields.get("iterator").cloned().unwrap_or_default();
          let generator = iterator.generator?;
          reach = reach.join(self.state.ret(generator).effect_of());
          continue;
        }
        Some(Primitive::Async) => {
          // The fiber resumes with the Effect `register` hands to `resume`.
          let register = fields.get("register").cloned().unwrap_or_default();
          let cell = Rc::new(std::cell::RefCell::new(Object::default()));
          let resume =
            Value { callables: vec![Callable::Resolve(Rc::clone(&cell))], ..Value::default() };
          self.call_value_def(
            &register,
            &Value::unknown(),
            &[resume, Value::unknown()],
            depth + 1,
          )?;
          let resumed = cell.borrow().other.clone();
          reach = reach.join(self.to_reach_seen(&resumed, depth + 1)?.success().effect_of());
          continue;
        }
        Some(Primitive::OnFailure | Primitive::OnSuccessAndFailure) => {
          let input = field(&self.defs.args_key)?;
          let input_success = self.to_reach_seen(&input, depth + 1)?.success();
          let this = Value { objects: vec![Rc::clone(object)], ..Value::default() };
          let mut success = Reach::NONE;
          if primitive == Some(Primitive::OnFailure) {
            success = success.join(input_success);
          } else if let Some(cont) = field(&self.defs.cont_key) {
            let ret = self.call_value_def(
              &cont,
              &this,
              &[Value::reach(input_success), fiber.clone()],
              depth + 1,
            )?;
            success = success.join(self.to_reach_seen(&ret, depth + 1)?.success());
          }
          if let Some(handler) = field(&self.defs.cont_e_key) {
            let ret = self.call_value_def(&handler, &this, &[cause, fiber], depth + 1)?;
            success = success.join(self.to_reach_seen(&ret, depth + 1)?.success());
          }
          reach = reach.join(success.effect_of());
          continue;
        }
        _ => {}
      }
      match primitive {
        Some(Primitive::OnSuccess) => {
          let input = fields.get(self.defs.args_key.as_deref()?)?.clone();
          let cont = fields.get(self.defs.cont_key.as_deref()?)?.clone();
          let input_success = self.to_reach_seen(&input, depth + 1)?.success();
          let this = Value { objects: vec![Rc::clone(object)], ..Value::default() };
          let ret =
            self.call_value_def(&cont, &this, &[Value::reach(input_success), fiber], depth + 1)?;
          reach = reach.join(self.to_reach_seen(&ret, depth + 1)?.success().effect_of());
        }
        Some(_) => return None,
        None => {
          let mut getters: Vec<Value<'s, 'a>> = object.borrow().getters.values().cloned().collect();
          let class = object.borrow().class;
          if let Some((module, class)) = class {
            getters.extend(self.class_getters(module, class));
          }
          let this = Value { objects: vec![Rc::clone(object)], ..Value::default() };
          for getter in getters {
            let value = self.call_value_def(&getter, &this, &[], depth + 1)?;
            reach = reach.join(self.to_reach_within(&value, depth + 1, seen)?);
          }
          for field in fields.values().chain(std::iter::once(&other)) {
            if !field.callables.is_empty() && !field.reach.any() {
              continue;
            }
            reach = reach.join(self.to_reach_within(field, depth + 1, seen)?);
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
    let flow = self.exec_statement_inner(ctx, statement, depth);
    if flow.is_none() && self.state.derive_stop.is_none() {
      self.state.derive_stop = Some((ctx.module, statement.span().start));
    }
    flow
  }

  #[expect(clippy::too_many_lines, reason = "one arm per statement kind")]
  fn exec_statement_inner(
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
        let thrown = self.thrown_reach_exact(&value, depth)?;
        #[expect(clippy::print_stderr, reason = "opt-in debugging of the experimental pass")]
        if thrown.any() && std::env::var_os("ROLLDOWN_EFFECT_SERVICE_REACH_TRACE").is_some() {
          eprintln!(
            "[reach] Effect code throws {thrown:?} (thrown before {:?}) at module {:?} {}: fiber={} objects={} reach={:?}",
            self.state.thrown,
            ctx.module,
            statement.span.start,
            value.fiber,
            value.objects.len(),
            value.reach
          );
        }
        self.join_thrown(thrown, "throw in Effect code");
        Flow::default()
      }
      Statement::BreakStatement(statement) if statement.label.is_none() => {
        Flow { broke: true, ..Flow::default() }
      }
      Statement::VariableDeclaration(declaration) => {
        for declarator in &declaration.declarations {
          let value = match &declarator.init {
            Some(init) => self.eval_def(ctx, init, depth)?,
            None => Value::constant(Constant::Undefined),
          };
          self.bind_def(ctx, &declarator.id, value, depth)?;
        }
        through()
      }
      Statement::ContinueStatement(statement) if statement.label.is_none() => Flow::default(),
      Statement::ForStatement(statement) => {
        if let Some(init) = &statement.init {
          match init {
            oxc::ast::ast::ForStatementInit::VariableDeclaration(declaration) => {
              for declarator in &declaration.declarations {
                let value = match &declarator.init {
                  Some(init) => self.eval_def(ctx, init, depth)?,
                  None => Value::constant(Constant::Undefined),
                };
                self.bind_def(ctx, &declarator.id, value, depth)?;
              }
            }
            init => {
              self.eval_def(ctx, init.as_expression()?, depth)?;
            }
          }
        }
        self.run_loop(ctx, depth, |walker, ctx, depth| {
          let test = match &statement.test {
            Some(test) => walker.eval_test(ctx, test, depth),
            None => None,
          };
          if test == Some(false) {
            return Some(None);
          }
          let flow = walker.exec_statement(ctx, &statement.body, depth)?;
          if let Some(update) = &statement.update {
            walker.eval_def(ctx, update, depth)?;
          }
          Some(Some(flow))
        })?
      }
      Statement::WhileStatement(statement) => self.run_loop(ctx, depth, |walker, ctx, depth| {
        if walker.eval_test(ctx, &statement.test, depth) == Some(false) {
          return Some(None);
        }
        walker.exec_statement(ctx, &statement.body, depth).map(Some)
      })?,
      Statement::DoWhileStatement(statement) => {
        self.run_loop(ctx, depth, |walker, ctx, depth| {
          let flow = walker.exec_statement(ctx, &statement.body, depth)?;
          walker.eval_test(ctx, &statement.test, depth);
          Some(Some(flow))
        })?
      }
      Statement::ForOfStatement(statement) => {
        let iterable = self.eval_def(ctx, &statement.right, depth)?;
        let element = self.element_def(&iterable);
        self.run_loop(ctx, depth, |walker, ctx, depth| {
          walker.bind_def_for_left(ctx, &statement.left, &element, depth)?;
          walker.exec_statement(ctx, &statement.body, depth).map(Some)
        })?
      }
      Statement::ForInStatement(statement) => {
        let object = self.eval_def(ctx, &statement.right, depth)?;
        if object.reach.value() {
          self.event(ReachEvent::All("enumerated with for...in in Effect's code"), statement.span);
        }
        self.run_loop(ctx, depth, |walker, ctx, depth| {
          walker.bind_def_for_left(ctx, &statement.left, &Value::unknown(), depth)?;
          walker.exec_statement(ctx, &statement.body, depth).map(Some)
        })?
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
            ctx.frame.declare((ctx.module, id.symbol_id.get()?), self.cause_value());
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

  /// Binds a declaration or parameter pattern.
  fn bind_def(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    pattern: &'s BindingPattern<'a>,
    value: Value<'s, 'a>,
    depth: usize,
  ) -> Option<()> {
    match pattern {
      BindingPattern::BindingIdentifier(id) => {
        ctx.frame.declare((ctx.module, id.symbol_id.get()?), value);
      }
      BindingPattern::AssignmentPattern(pattern) => {
        let default = self.eval_def(ctx, &pattern.right, depth)?;
        self.bind_def(ctx, &pattern.left, value.join(default), depth)?;
      }
      BindingPattern::ArrayPattern(pattern) => {
        for (index, element) in pattern.elements.iter().enumerate() {
          let Some(element) = element else { continue };
          let item = self.member_def(&value, Some(&index.to_string()), pattern.span, depth);
          self.bind_def(ctx, element, item, depth)?;
        }
        if let Some(rest) = &pattern.rest {
          let mut data = Object { kind: ObjectKind::Array, ..Object::default() };
          data.other = self.element_def(&value);
          self.bind_def(ctx, &rest.argument, Value::object(data), depth)?;
        }
      }
      BindingPattern::ObjectPattern(pattern) => {
        for property in &pattern.properties {
          let key = if property.computed {
            let key = self.eval_def(ctx, property.key.as_expression()?, depth)?;
            key.constant.as_ref().and_then(Constant::key)
          } else {
            key_name(&property.key)
          };
          let item = self.member_def(&value, key.as_deref(), property.span, depth);
          self.bind_def(ctx, &property.value, item, depth)?;
        }
        if pattern.rest.is_some() {
          return None;
        }
      }
    }
    Some(())
  }

  fn bind_def_for_left(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    left: &'s oxc::ast::ast::ForStatementLeft<'a>,
    value: &Value<'s, 'a>,
    depth: usize,
  ) -> Option<()> {
    match left {
      oxc::ast::ast::ForStatementLeft::VariableDeclaration(declaration) => {
        for declarator in &declaration.declarations {
          self.bind_def(ctx, &declarator.id, value.clone(), depth)?;
        }
        Some(())
      }
      left => self.assign_def(ctx, left.as_assignment_target()?, value, depth),
    }
  }

  /// Runs a loop body until what it binds stops growing (bounded): the body
  /// returns `None` when it cannot be evaluated and `Some(None)` when the
  /// loop does not run.
  fn run_loop(
    &mut self,
    ctx: &Ctx<'s, 'a>,
    depth: usize,
    mut body: impl FnMut(&mut Self, &Ctx<'s, 'a>, usize) -> Option<Option<Flow<'s, 'a>>>,
  ) -> Option<Flow<'s, 'a>> {
    let mut flow = Flow { falls_through: true, ..Flow::default() };
    for _ in 0..3 {
      match body(self, ctx, depth)? {
        None => break,
        Some(iteration) => flow.returns = std::mem::take(&mut flow.returns).join(iteration.returns),
      }
    }
    Some(flow)
  }

  /// What a thrown value or failure cause may reach: `None` when a part
  /// cannot be read, so the derivation stops and the call falls back. A
  /// value Effect's code built without the program's values carries none.
  fn thrown_reach_exact(&mut self, value: &Value<'s, 'a>, depth: usize) -> Option<Reach> {
    if !value.carries_program() {
      return Some(Reach::NONE);
    }
    self.to_reach(value, depth).map(Reach::collapse)
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
      // `exitSucceed(value)`: an Exit that succeeds with `value`.
      Primitive::Success => self.exit_object(first),
      // `exitFailCause(cause)`: fails; error handlers may receive the cause.
      Primitive::Failure => {
        let thrown = self.thrown_reach_exact(&first, depth + 1)?;
        self.join_thrown(thrown, "Failure primitive");
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
      // Built with `new` or `Object.create` from their prototype.
      Primitive::OnSuccess
      | Primitive::OnFailure
      | Primitive::OnSuccessAndFailure
      | Primitive::Iterator
      | Primitive::Async => return None,
      Primitive::While => {
        let options = first;
        let condition = self.member_def(&options, Some("while"), Span::default(), depth);
        let body = self.member_def(&options, Some("body"), Span::default(), depth);
        let step = self.member_def(&options, Some("step"), Span::default(), depth);
        for _ in 0..3 {
          self.call_value_def(&condition, &options, &[], depth + 1)?;
          let effect = self.call_value_def(&body, &options, &[], depth + 1)?;
          let success = Value::reach(self.to_reach(&effect, depth + 1)?.success());
          self.call_value_def(&step, &options, &[success], depth + 1)?;
        }
        Value::default()
      }
      // `exitPrimitive(effect)`: succeeds with the input's Exit.
      Primitive::Exit => {
        let mut fields = FxHashMap::default();
        if let Some(key) = &self.defs.args_key {
          fields.insert(key.clone(), first);
        }
        Value::object(Object { fields, primitive: Some(Primitive::Exit), ..Object::default() })
      }
    })
  }
}
