//! Constraint generation: JavaScript semantics only.
//!
//! Every module's AST is walked once. Top-level code is installed immediately;
//! the constraints of a function body are kept pending on its `FnId` and
//! installed when the solver finds the function reachable. Nothing here knows
//! about Effect: symbols bound to a model rule (`rule_symbols`) are seeded with
//! the rule's builtin object and their initialisers are skipped.

use arcstr::ArcStr;
use oxc::ast::ast::{
  Argument, ArrayExpressionElement, ArrowFunctionBody, ArrowFunctionExpression,
  AssignmentExpression, AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty,
  BindingIdentifier, BindingPattern, CallExpression, ChainElement, Class, ClassElement,
  ComputedMemberExpression, Declaration, ExportDefaultDeclarationKind, Expression,
  ForStatementInit, ForStatementLeft, FormalParameters, Function, IdentifierReference,
  ImportDeclarationSpecifier, MethodDefinitionKind, ObjectExpression, ObjectPropertyKind, Program,
  PropertyKey, PropertyKind, SimpleAssignmentTarget, Statement, StaticMemberExpression,
  VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
};
use oxc::semantic::{ReferenceId, Scoping};
use oxc::span::GetSpan;
use oxc::syntax::operator::{AssignmentOperator, BinaryOperator};
use oxc_index::IndexVec;
use rolldown_common::{ModuleIdx, SymbolRef, SymbolRefDb};
use rustc_hash::{FxHashMap, FxHashSet};
use std::rc::Rc;

use super::heap::{
  Accessor, Analysis, Arg, Call, Constraint, Flavour, FnId, FnInfo, FnKind, Key, ObjId, ObjKind,
  Pending, Protocol, Site, VarId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
  Function,
  Param,
  Var,
  Const,
  Let,
  Class,
  Import,
}

#[derive(Clone, Debug)]
pub struct DeclInfo {
  pub kind: DeclKind,
  /// Offset after which reading the binding cannot throw a TDZ error: the end of its
  /// declarator, of the parameter list, or of the loop head's right-hand side.
  pub initialized_after: u32,
  /// Innermost `switch` case containing the declaration, if any.
  pub case: Option<u32>,
  pub func: FnId,
}

#[derive(Clone, Debug)]
pub enum Shape {
  /// A function or arrow literal.
  FnLiteral,
  /// An identifier reference.
  Ident(SymbolRef),
  /// A call whose callee is in `callee` and whose arguments are all simple (literals,
  /// function literals, safe identifiers).
  Call {
    callee: VarId,
    simple_args: bool,
  },
  Other,
}

#[derive(Clone, Debug)]
pub struct PropInfo {
  pub span_start: u32,
  pub name: Option<ArcStr>,
  pub value: VarId,
  pub shape: Shape,
  pub accessor: bool,
  pub spread: bool,
}

#[derive(Clone, Debug)]
pub struct LiteralInfo {
  pub module: ModuleIdx,
  pub span_start: u32,
  pub func: FnId,
  /// Innermost `switch` case containing the literal, if any.
  pub case: Option<u32>,
  pub props: Vec<PropInfo>,
}

pub struct Shared<'l> {
  pub symbols: &'l SymbolRefDb,
  pub symbol_vars: FxHashMap<SymbolRef, VarId>,
  pub namespace_syms: FxHashMap<SymbolRef, ModuleIdx>,
  pub namespace_objs: FxHashMap<ModuleIdx, ObjId>,
  pub rule_symbols: FxHashMap<SymbolRef, ObjId>,
  pub externals: FxHashMap<ModuleIdx, ObjId>,
  pub globals: FxHashMap<ArcStr, VarId>,
  pub literals: FxHashMap<ObjId, LiteralInfo>,
  pub decls: FxHashMap<SymbolRef, DeclInfo>,
  pub thrown: VarId,
  pub prim: VarId,
  pub top_host: VarId,
  pub top_ext: VarId,
  pub module_fns: IndexVec<ModuleIdx, Option<FnId>>,
  /// Modules whose exports are reached through CommonJS interop.
  pub cjs_modules: FxHashSet<ModuleIdx>,
  /// The value of each module hole (`LinkStage::module_hole`): what its exports, its
  /// namespace and a `require` of it give.
  pub hole_values: FxHashMap<ModuleIdx, ObjId>,
  /// Lazy top-level statements: the canonical symbol each declares -> its body.
  pub lazy_decls: FxHashMap<SymbolRef, FnId>,
  /// Every symbol reference: (the body it appears in, canonical symbol).
  pub lazy_refs: Vec<(FnId, SymbolRef)>,
  /// Each module's lazy top-level statements.
  pub lazy_by_module: FxHashMap<ModuleIdx, Vec<FnId>>,
  /// Resolves an unresolved global name to a variable, via the host table.
  pub resolve_global: fn(&mut Analysis, &mut FxHashMap<ArcStr, VarId>, &str) -> VarId,
  /// Canonical symbols declared, by a `const` that is never reassigned, as a key constructor's
  /// result (`const K = Context.Service(..)`), for the key-class test, with the position of the
  /// top-level statement declaring each (`Gen::key_heritage`).
  pub key_symbols: FxHashMap<SymbolRef, u32>,
  /// Top-level statements numbered so far, in generation order (modules in execution order).
  pub statements: u32,
  /// Each lazy statement: its module, span and body, for the decision record.
  pub lazy_spans: Vec<(ModuleIdx, u32, u32, FnId)>,
  /// Lazy statements generated up front although deferral was on (`may_bail`).
  pub kept_eager: usize,
  /// Time spent registering deferred statements and scanning them (`Gen::program`).
  pub scan_time: std::time::Duration,
  /// Canonical symbols bound to effect's own `Context.Service`/`Context.Reference` rules.
  pub key_ctor_symbols: FxHashSet<SymbolRef>,
  /// Modules exporting one of those under its own name (`Context.Service` through a namespace).
  pub key_ctor_members: FxHashSet<(ModuleIdx, &'static str)>,
}

impl Shared<'_> {
  pub fn symbol_var(&mut self, a: &mut Analysis, sym: SymbolRef) -> VarId {
    let canonical = self.symbols.canonical_ref_for(sym);
    if let Some(v) = self.symbol_vars.get(&canonical) {
      return *v;
    }
    let v = a.var();
    self.symbol_vars.insert(canonical, v);
    if let Some(obj) = self.rule_symbols.get(&canonical) {
      a.seed(v, *obj);
    }
    if let Some(module) = self.namespace_syms.get(&canonical) {
      let obj = self.namespace_objs[module];
      a.seed(v, obj);
    }
    v
  }
}

/// Per-module import binding facts, computed by `mod.rs`.
pub struct ModuleFacts<'l> {
  pub module: ModuleIdx,
  pub scoping: &'l Scoping,
  /// Import binding symbol -> (external object, imported name or None for the namespace)
  pub external_imports: FxHashMap<SymbolRef, (ObjId, Option<ArcStr>)>,
  /// Import binding symbol -> module index, for imports from CommonJS modules.
  pub cjs_imports: FxHashSet<SymbolRef>,
  /// Dynamic `import()` node ids -> resolved module (None: external or unresolvable).
  pub dynamic_imports: FxHashMap<usize, Option<ModuleIdx>>,
  pub default_export: SymbolRef,
  /// Lazy top-level statements (by body index) and the symbols they declare (see
  /// `Gen::program`).
  pub lazy_stmts: FxHashMap<usize, Vec<SymbolRef>>,
}

pub struct Gen<'a, 'l, 's> {
  pub a: &'a mut Analysis,
  pub sh: &'s mut Shared<'l>,
  pub m: &'s ModuleFacts<'l>,
  cur: FnId,
  /// Enclosing non-arrow functions, for `this`.
  this_stack: Vec<VarId>,
  /// Enclosing class heritage variables, for `super`.
  class_stack: Vec<Option<VarId>>,
  /// The lazy top-level statement being generated, if any.
  lazy_owner: Option<FnId>,
  generator_stack: Vec<bool>,
  module_fn: FnId,
  /// End offset until which the binding being declared is uninitialised.
  tdz_until: Option<u32>,
  /// Innermost `switch` case ids.
  case_stack: Vec<u32>,
  next_case: u32,
  /// Values thrown inside the enclosing inert `try` blocks (see `inert_statement`).
  try_throws: Vec<VarId>,
  /// The position of the top-level statement being generated (`Shared::statements`).
  seq: u32,
  /// Key symbols this top-level statement declared so far (`key_heritage`).
  stmt_keys: Vec<SymbolRef>,
  /// The allocation budget ran out (`checkpoint`): every statement and expression from here on
  /// returns at once, and loops over a node's children stop.
  stopped: bool,
}

/// The analysis allocates as usual again once generation is over (`Analysis::generating`).
impl Drop for Gen<'_, '_, '_> {
  fn drop(&mut self) {
    self.a.generating = false;
  }
}

/// A lazy top-level statement whose generation is deferred to its first activation: its body,
/// its index in the module's program and its position (`Shared::statements`).
#[derive(Clone, Copy, Debug)]
pub struct DeferredStatement {
  pub lazy: FnId,
  pub index: usize,
  pub seq: u32,
}

impl<'a, 'l, 's> Gen<'a, 'l, 's> {
  pub fn new(a: &'a mut Analysis, sh: &'s mut Shared<'l>, m: &'s ModuleFacts<'l>) -> Self {
    let module_fn = Self::new_fn(a, m.module, 0, FnKind::Normal);
    sh.module_fns[m.module] = Some(module_fn);
    let prim = sh.prim;
    a.copy(prim, a.fns[module_fn].this);
    Self::at_top(a, sh, m, module_fn, 0)
  }

  /// A generator for the deferred statements of a module `program` already walked: it
  /// continues at the module's top level, with the module's `switch` cases numbered on from
  /// `next_case`.
  pub fn resume(
    a: &'a mut Analysis,
    sh: &'s mut Shared<'l>,
    m: &'s ModuleFacts<'l>,
    next_case: u32,
  ) -> Self {
    let module_fn = sh.module_fns[m.module].expect("the module's top level was generated");
    Self::at_top(a, sh, m, module_fn, next_case)
  }

  fn at_top(
    a: &'a mut Analysis,
    sh: &'s mut Shared<'l>,
    m: &'s ModuleFacts<'l>,
    module_fn: FnId,
    next_case: u32,
  ) -> Self {
    let this = a.fns[module_fn].this;
    let stopped = a.generation_stopped.is_some();
    a.generating = true;
    Self {
      a,
      sh,
      m,
      cur: module_fn,
      this_stack: vec![this],
      class_stack: vec![],
      lazy_owner: None,
      generator_stack: vec![false],
      module_fn,
      tdz_until: None,
      case_stack: vec![],
      next_case,
      try_throws: vec![],
      seq: 0,
      stmt_keys: vec![],
      stopped,
    }
  }

  /// The next `switch` case number, for `resume`.
  pub fn next_case(&self) -> u32 {
    self.next_case
  }

  fn new_fn(a: &mut Analysis, module: ModuleIdx, span_start: u32, kind: FnKind) -> FnId {
    let this = a.var();
    let ret = a.var();
    let yielded_fail = a.var();
    let yielded = a.var();
    let arguments_elem = a.var();
    a.fns.push(FnInfo::new(
      module,
      span_start,
      kind,
      [this, ret, yielded_fail, yielded, arguments_elem],
    ))
  }

  pub fn module_fn(&self) -> FnId {
    self.module_fn
  }

  // == emission helpers

  fn site(&self, start: u32) -> Site {
    Site { module: self.m.module, start, sub: 0 }
  }

  fn emit(&mut self, p: Pending) {
    if !self.a.generation_exhausted() {
      self.a.fns[self.cur].pending.push(p);
    }
  }

  /// Adds a copy to the analysis now rather than when the function is reached, unless the
  /// budget is over (`Analysis::generation_exhausted`).
  fn install(&mut self, src: VarId, dst: VarId) {
    if !self.a.generation_exhausted() {
      self.a.copy(src, dst);
    }
  }

  fn copy(&mut self, src: VarId, dst: VarId) {
    if src != dst {
      self.emit(Pending::Copy(src, dst));
    }
  }

  fn constrain(&mut self, var: VarId, c: Constraint) {
    self.emit(Pending::Constraint(var, c));
  }

  fn seed(&mut self, var: VarId, obj: ObjId) {
    self.emit(Pending::Seed(var, obj));
  }

  /// Checks the allocation budget before a statement, expression or other child node at `start`
  /// is generated: every loop over a node's children checks it, including the walks that never
  /// enter an expression (parameters, binding and assignment patterns, class members), and so
  /// does each level of a walk that recurses without a loop (nested patterns, `super`'s
  /// heritages).
  /// Once it has run out, generation stops for the rest of the solve: what was generated stays
  /// partial, and the solve ends at once keeping everything (`Analysis::solve_with`), so no
  /// partial body is ever pruned against. This bounds how far generation walks; what the nodes
  /// it is inside allocate as they complete is bounded by `Analysis::generation_exhausted`.
  fn checkpoint(&mut self, start: u32) -> bool {
    if !self.stopped && self.a.over_budget() {
      self.stopped = true;
      self.a.generation_stopped = Some(self.site(start));
    }
    self.stopped
  }

  fn fresh(&mut self) -> VarId {
    let sink = self.a.generation_exhausted();
    let v = self.a.var();
    if self.cur != self.module_fn && !sink {
      self.a.fns[self.cur].locals.push(v);
    }
    v
  }

  /// A string built at run time: host data, or a string known to start with a constant
  /// (`Analysis::prefixed_host`). An addition with no constant string at its start may be a
  /// number, which is host data too.
  fn run_time_string(&mut self, expr: &Expression<'_>) -> VarId {
    match Self::const_prefix(expr) {
      Some(prefix) if !prefix.is_empty() => {
        let obj = self.a.prefixed_host(&prefix);
        let v = self.fresh();
        self.seed(v, obj);
        v
      }
      _ => self.sh.top_host,
    }
  }

  /// The constant start of a string expression: all of a constant string, the first part of a
  /// template literal, or the constant start of an addition's left side (followed by its right
  /// side's when the left side is constant).
  fn const_prefix(expr: &Expression<'_>) -> Option<String> {
    if let Some(s) = Self::const_string_of(expr) {
      return Some(s);
    }
    match expr {
      Expression::TemplateLiteral(t) => {
        Some(t.quasis.first()?.value.cooked.as_ref()?.as_str()?.to_string())
      }
      Expression::BinaryExpression(b) if b.operator == BinaryOperator::Addition => {
        match Self::const_string_of(&b.left) {
          Some(l) => Some(l + &Self::const_prefix(&b.right).unwrap_or_default()),
          None => Self::const_prefix(&b.left),
        }
      }
      Expression::ParenthesizedExpression(p) => Self::const_prefix(&p.expression),
      _ => None,
    }
  }

  fn str_var(&mut self, s: &str) -> VarId {
    let obj = self.a.str(s);
    let v = self.fresh();
    self.seed(v, obj);
    v
  }

  fn alloc(&mut self, kind: ObjKind, start: u32) -> (ObjId, VarId) {
    let obj = self.a.alloc(kind, Some(self.site(start)));
    let v = self.fresh();
    // Seeded now, not when the enclosing function is reached: the generator reads these sets
    // (class members, accessors) and a fresh variable holding an allocation has no effect on
    // its own. Also pending, for the clones of the function (where `v` is renamed).
    self.a.seed(v, obj);
    if self.cur != self.module_fn {
      self.seed(v, obj);
    }
    (obj, v)
  }

  fn load(&mut self, base: VarId, key: Key) -> VarId {
    let dst = self.fresh();
    self.constrain(base, Constraint::Load { key, dst });
    dst
  }

  fn store(&mut self, base: VarId, key: Key, src: VarId) {
    self.constrain(base, Constraint::Store { key, src });
  }

  fn symbol_var(&mut self, sym: SymbolRef) -> VarId {
    // Recorded so that reaching this body activates the lazy statement declaring `sym`.
    let owner =
      if self.cur == self.module_fn { self.lazy_owner.unwrap_or(self.cur) } else { self.cur };
    let canonical = self.sh.symbols.canonical_ref_for(sym);
    self.sh.lazy_refs.push((owner, canonical));
    self.sh.symbol_var(self.a, sym)
  }

  fn binding_var(&mut self, ident: &BindingIdentifier<'_>) -> Option<VarId> {
    let symbol = ident.symbol_id.get()?;
    Some(self.symbol_var((self.m.module, symbol).into()))
  }

  fn record_decl(&mut self, ident: &BindingIdentifier<'_>, kind: DeclKind) {
    if let Some(symbol) = ident.symbol_id.get() {
      let sym: SymbolRef = (self.m.module, symbol).into();
      let canonical = self.sh.symbols.canonical_ref_for(sym);
      let initialized_after = match kind {
        DeclKind::Function | DeclKind::Var => 0,
        _ => self.tdz_until.unwrap_or(ident.span.end),
      };
      self.sh.decls.insert(
        canonical,
        DeclInfo { kind, initialized_after, case: self.case_stack.last().copied(), func: self.cur },
      );
      if self.cur != self.module_fn {
        let v = self.symbol_var(sym);
        self.a.fns[self.cur].locals.push(v);
      }
    }
  }

  fn reference_var(&mut self, ident: &IdentifierReference<'_>) -> VarId {
    self.reference_parts(ident.reference_id.get(), ident.name.as_str(), ident.span.start)
  }

  fn reference_parts(&mut self, reference: Option<ReferenceId>, name: &str, start: u32) -> VarId {
    let symbol = reference.and_then(|id| self.m.scoping.get_reference(id).symbol_id());
    match symbol {
      Some(symbol) => {
        let sym: SymbolRef = (self.m.module, symbol).into();
        if let Some((ext, name)) = self.m.external_imports.get(&sym).cloned() {
          let v = self.symbol_var(sym);
          match name {
            None => self.seed(v, ext),
            Some(name) => {
              let holder = self.fresh();
              self.seed(holder, ext);
              let loaded = self.load(holder, Key::Name(name));
              self.copy(loaded, v);
            }
          }
          return v;
        }
        if self.m.cjs_imports.contains(&sym) {
          let v = self.symbol_var(sym);
          let top = self.sh.top_ext;
          self.copy(top, v);
          return v;
        }
        self.symbol_var(sym)
      }
      None => match name {
        "undefined" | "NaN" | "Infinity" => self.sh.prim,
        "arguments" => {
          if self.this_stack.len() > 1 {
            let elem = self.a.fns[self.cur].arguments_elem;
            let (arr, v) = self.alloc(ObjKind::Array, start);
            let arr_elem = self.a.elem(arr);
            self.copy(elem, arr_elem);
            v
          } else {
            self.sh.top_host
          }
        }
        _ => (self.sh.resolve_global)(self.a, &mut self.sh.globals, name),
      },
    }
  }

  // == program

  /// Generates the module's top level. A lazy statement's constraints wait in a body of their
  /// own, like a function's, and are installed once reachable code references a symbol it
  /// declares (`LinkStage::effect_service_shaking` adds the activations once every module is
  /// generated). With `defer`, a lazy statement's body is only registered here and generated
  /// when the solver first installs it (`deferred_statement`); the statements returned are
  /// those. What other statements' generation or the plan reads of one is produced here
  /// either way: its key symbols (`scan_key_symbols`), and the unsafe-lookup bailout, by
  /// generating any statement that may reach it now (`may_bail`).
  pub fn program(&mut self, program: &Program<'_>, defer: bool) -> Vec<DeferredStatement> {
    let mut deferred = vec![];
    for (i, stmt) in program.body.iter().enumerate() {
      self.begin_statement();
      let Some(declared) = self.m.lazy_stmts.get(&i).cloned() else {
        self.statement(stmt);
        continue;
      };
      let span = oxc::span::GetSpan::span(stmt);
      let lazy = Self::new_fn(self.a, self.m.module, span.start, FnKind::Normal);
      self.a.fns[lazy].leaf = false;
      for sym in declared {
        self.sh.lazy_decls.insert(sym, lazy);
      }
      self.sh.lazy_by_module.entry(self.m.module).or_default().push(lazy);
      self.sh.lazy_spans.push((self.m.module, span.start, span.end, lazy));
      if defer {
        let scan = std::time::Instant::now();
        let bails = may_bail(stmt);
        if !bails {
          self.scan_key_symbols(stmt);
          self.a.fns[lazy].deferred = true;
          deferred.push(DeferredStatement { lazy, index: i, seq: self.seq });
        }
        self.sh.scan_time += scan.elapsed();
        if !bails {
          continue;
        }
        self.sh.kept_eager += 1;
      }
      self.lazy_statement(lazy, stmt);
    }
    deferred
  }

  /// Generates a deferred statement (`program`), on its first activation.
  pub fn deferred_statement(&mut self, d: DeferredStatement, stmt: &Statement<'_>) {
    self.seq = d.seq;
    self.stmt_keys.clear();
    self.lazy_statement(d.lazy, stmt);
  }

  fn begin_statement(&mut self) {
    self.seq = self.sh.statements;
    self.sh.statements += 1;
    self.stmt_keys.clear();
  }

  fn lazy_statement(&mut self, lazy: FnId, stmt: &Statement<'_>) {
    let before = self.a.fns[self.module_fn].pending.len();
    self.lazy_owner = Some(lazy);
    self.statement(stmt);
    self.lazy_owner = None;
    let moved: Vec<Pending> = self.a.fns[self.module_fn].pending.drain(before..).collect();
    debug_assert!(self.a.fns[lazy].pending.is_empty(), "a lazy statement is generated once");
    self.a.fns[lazy].pending = moved;
  }

  /// The key symbols a deferred top-level `const` declares, as `variable_declaration` records
  /// them, for the statements generated before it (`key_heritage`).
  fn scan_key_symbols(&mut self, stmt: &Statement<'_>) {
    let d = match stmt {
      Statement::VariableDeclaration(d) => d,
      Statement::ExportDeclaration(e) => match &e.declaration {
        Declaration::VariableDeclaration(d) => d,
        _ => return,
      },
      _ => return,
    };
    for decl in &d.declarations {
      if self.bound_to_rule(decl).is_some() {
        continue;
      }
      if let Some(key) = self.key_symbol(d, decl) {
        self.sh.key_symbols.entry(key).or_insert(self.seq);
      }
    }
  }

  /// The declarator's binding when a model rule is bound to it (`variable_declaration`).
  fn bound_to_rule<'d>(
    &self,
    decl: &'d VariableDeclarator<'_>,
  ) -> Option<(&'d BindingIdentifier<'d>, SymbolRef)> {
    let BindingPattern::BindingIdentifier(id) = &decl.id else { return None };
    let sym: SymbolRef = (self.m.module, id.symbol_id.get()?).into();
    let canonical = self.sh.symbols.canonical_ref_for(sym);
    self.sh.rule_symbols.contains_key(&canonical).then_some((&**id, sym))
  }

  /// The canonical symbol a `const` declarator binds to a key constructor's result.
  fn key_symbol(
    &self,
    d: &VariableDeclaration<'_>,
    decl: &VariableDeclarator<'_>,
  ) -> Option<SymbolRef> {
    let (BindingPattern::BindingIdentifier(id), Some(init)) = (&decl.id, &decl.init) else {
      return None;
    };
    let symbol = id.symbol_id.get()?;
    (d.kind == VariableDeclarationKind::Const && self.key_constructor_call(init))
      .then(|| self.sh.symbols.canonical_ref_for((self.m.module, symbol).into()))
  }

  fn statements(&mut self, stmts: &[Statement<'_>]) {
    for s in stmts {
      if self.stopped {
        break;
      }
      self.statement(s);
    }
  }

  fn statement(&mut self, stmt: &Statement<'_>) {
    if self.checkpoint(oxc::span::GetSpan::span(stmt).start) {
      return;
    }
    match stmt {
      Statement::BlockStatement(b) => self.statements(&b.body),
      Statement::DoWhileStatement(s) => {
        self.statement(&s.body);
        self.expr(&s.test);
      }
      Statement::ExpressionStatement(s) => {
        self.expr(&s.expression);
      }
      Statement::ForInStatement(s) => {
        let right = self.expr(&s.right);
        self.constrain(right, Constraint::Enumerate);
        let key = self.sh.top_host;
        let saved = self.tdz_until.replace(s.right.span().end);
        self.for_left(&s.left, key);
        self.tdz_until = saved;
        self.statement(&s.body);
      }
      Statement::ForOfStatement(s) => {
        let right = self.expr(&s.right);
        let elem = self.fresh();
        self.constrain(right, Constraint::ElemOf(elem));
        let value = if s.r#await {
          let v = self.fresh();
          self.constrain(elem, Constraint::AwaitOf(v));
          v
        } else {
          elem
        };
        let saved = self.tdz_until.replace(s.right.span().end);
        self.for_left(&s.left, value);
        self.tdz_until = saved;
        self.statement(&s.body);
      }
      Statement::ForStatement(s) => {
        if let Some(init) = &s.init {
          match init {
            ForStatementInit::VariableDeclaration(d) => self.variable_declaration(d),
            other => {
              if let Some(e) = other.as_expression() {
                self.expr(e);
              }
            }
          }
        }
        if let Some(t) = &s.test {
          self.expr(t);
        }
        if let Some(u) = &s.update {
          self.expr(u);
        }
        self.statement(&s.body);
      }
      Statement::IfStatement(s) => {
        self.expr(&s.test);
        self.statement(&s.consequent);
        if let Some(alt) = &s.alternate {
          self.statement(alt);
        }
      }
      Statement::LabeledStatement(s) => self.statement(&s.body),
      Statement::ReturnStatement(s) => {
        if let Some(arg) = &s.argument {
          let v = self.expr(arg);
          let ret = self.a.fns[self.cur].ret;
          self.copy(v, ret);
        }
      }
      Statement::SwitchStatement(s) => {
        self.expr(&s.discriminant);
        for case in &s.cases {
          if self.checkpoint(case.span.start) {
            break;
          }
          if let Some(t) = &case.test {
            self.expr(t);
          }
          let id = self.next_case;
          self.next_case += 1;
          self.case_stack.push(id);
          self.statements(&case.consequent);
          self.case_stack.pop();
        }
      }
      Statement::ThrowStatement(s) => {
        let v = self.expr(&s.argument);
        let thrown = self.sh.thrown;
        self.copy(v, thrown);
        for t in self.try_throws.clone() {
          self.copy(v, t);
        }
      }
      Statement::TryStatement(s)
        if s.handler.as_ref().is_some_and(|h| h.param.is_some())
          && s.block.body.iter().all(inert_statement) =>
      {
        // Nothing in the block runs other code or reads a property, so the catch binding is
        // what the block's own `throw`s throw, or an Error the engine raises.
        let local = self.fresh();
        self.try_throws.push(local);
        self.statements(&s.block.body);
        self.try_throws.pop();
        let handler = s.handler.as_ref().expect("checked above");
        let param = handler.param.as_ref().expect("checked above");
        let (_, error) = self.alloc(ObjKind::Object, handler.span.start);
        self.copy(local, error);
        // A default in the pattern runs before the binding is initialised.
        let saved = self.tdz_until.replace(param.pattern.span().end);
        self.bind_pattern(&param.pattern, error, DeclKind::Let);
        self.tdz_until = saved;
        self.statements(&handler.body.body);
        if let Some(finalizer) = &s.finalizer {
          self.statements(&finalizer.body);
        }
      }
      Statement::TryStatement(s) => {
        self.statements(&s.block.body);
        if let Some(handler) = &s.handler {
          if let Some(param) = &handler.param {
            let thrown = self.sh.thrown;
            let v = self.fresh();
            self.copy(thrown, v);
            let top = self.sh.top_host;
            self.copy(top, v);
            let saved = self.tdz_until.replace(param.pattern.span().end);
            self.bind_pattern(&param.pattern, v, DeclKind::Let);
            self.tdz_until = saved;
          }
          self.statements(&handler.body.body);
        }
        if let Some(finalizer) = &s.finalizer {
          self.statements(&finalizer.body);
        }
      }
      Statement::WhileStatement(s) => {
        self.expr(&s.test);
        self.statement(&s.body);
      }
      Statement::WithStatement(s) => {
        self.expr(&s.object);
        self.statement(&s.body);
      }
      Statement::VariableDeclaration(d) => self.variable_declaration(d),
      Statement::FunctionDeclaration(f) => self.function_declaration(f),
      Statement::ClassDeclaration(c) => {
        let v = self.class(c);
        if let Some(id) = &c.id {
          self.record_decl(id, DeclKind::Class);
          if let Some(bv) = self.binding_var(id) {
            self.copy(v, bv);
          }
        }
      }
      Statement::ImportDeclaration(d) => {
        if let Some(specifiers) = &d.specifiers {
          for s in specifiers {
            let local = match s {
              ImportDeclarationSpecifier::ImportSpecifier(s) => &s.local,
              ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => &s.local,
              ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => &s.local,
            };
            self.record_decl(local, DeclKind::Import);
          }
        }
      }
      Statement::ExportDefaultDeclaration(d) => {
        let v = match &d.declaration {
          ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
            let v = self.function(f, FnKind::Normal);
            if let Some(id) = &f.id {
              self.record_decl(id, DeclKind::Function);
              if let Some(bv) = self.binding_var(id) {
                self.copy(v, bv);
              }
            }
            v
          }
          ExportDefaultDeclarationKind::ClassDeclaration(c) => {
            let v = self.class(c);
            if let Some(id) = &c.id {
              self.record_decl(id, DeclKind::Class);
              if let Some(bv) = self.binding_var(id) {
                self.copy(v, bv);
              }
            }
            v
          }
          other => match other.as_expression() {
            Some(e) => self.expr(e),
            None => return,
          },
        };
        let dv = self.symbol_var(self.m.default_export);
        self.copy(v, dv);
      }
      Statement::ExportDeclaration(d) => self.declaration(&d.declaration),
      _ => {}
    }
  }

  fn declaration(&mut self, decl: &Declaration<'_>) {
    match decl {
      Declaration::VariableDeclaration(d) => self.variable_declaration(d),
      Declaration::FunctionDeclaration(f) => self.function_declaration(f),
      Declaration::ClassDeclaration(c) => {
        let v = self.class(c);
        if let Some(id) = &c.id {
          self.record_decl(id, DeclKind::Class);
          if let Some(bv) = self.binding_var(id) {
            self.copy(v, bv);
          }
        }
      }
      _ => {}
    }
  }

  fn for_left(&mut self, left: &ForStatementLeft<'_>, value: VarId) {
    match left {
      ForStatementLeft::VariableDeclaration(d) => {
        for decl in &d.declarations {
          if self.checkpoint(decl.span.start) {
            break;
          }
          self.bind_pattern(&decl.id, value, decl_kind(d.kind));
        }
      }
      other => {
        if let Some(target) = other.as_assignment_target() {
          self.assign_target(target, value);
        }
      }
    }
  }

  fn function_declaration(&mut self, f: &Function<'_>) {
    // A declaration bound to a model rule keeps the rule and its body is not analysed, as a
    // bound variable's initialiser is not (`variable_declaration`).
    if let Some(id) = &f.id {
      if let Some(symbol) = id.symbol_id.get() {
        let sym: SymbolRef = (self.m.module, symbol).into();
        let canonical = self.sh.symbols.canonical_ref_for(sym);
        if self.sh.rule_symbols.contains_key(&canonical) {
          self.record_decl(id, DeclKind::Function);
          self.symbol_var(sym);
          return;
        }
      }
    }
    let v = self.function(f, FnKind::Normal);
    if let Some(id) = &f.id {
      self.record_decl(id, DeclKind::Function);
      if let Some(bv) = self.binding_var(id) {
        self.copy(v, bv);
      }
    }
  }

  fn variable_declaration(&mut self, d: &VariableDeclaration<'_>) {
    let kind = decl_kind(d.kind);
    for decl in &d.declarations {
      if self.checkpoint(decl.span.start) {
        break;
      }
      // A binding bound to a model rule keeps the rule and skips its initialiser.
      if let Some((id, sym)) = self.bound_to_rule(decl) {
        self.record_decl(id, kind);
        self.symbol_var(sym);
        continue;
      }
      if let Some(key) = self.key_symbol(d, decl) {
        self.sh.key_symbols.entry(key).or_insert(self.seq);
        self.stmt_keys.push(key);
      }
      let value = match &decl.init {
        Some(init) => self.expr(init),
        None => self.sh.prim,
      };
      let saved = self.tdz_until.replace(decl.span.end);
      self.bind_pattern(&decl.id, value, kind);
      self.tdz_until = saved;
    }
  }

  fn bind_pattern(&mut self, pattern: &BindingPattern<'_>, src: VarId, kind: DeclKind) {
    // Patterns nest with no sibling in between (`[...[...[...tail]]]`): each level checks.
    if !matches!(pattern, BindingPattern::BindingIdentifier(_))
      && self.checkpoint(oxc::span::GetSpan::span(pattern).start)
    {
      return;
    }
    match pattern {
      BindingPattern::BindingIdentifier(id) => {
        self.record_decl(id, kind);
        if let Some(v) = self.binding_var(id) {
          self.copy(src, v);
        }
      }
      BindingPattern::ObjectPattern(p) => {
        for prop in &p.properties {
          if self.checkpoint(prop.span.start) {
            break;
          }
          let key = self.property_key(&prop.key, prop.computed);
          let v = self.load(src, key);
          self.bind_pattern(&prop.value, v, kind);
        }
        if let Some(rest) = &p.rest {
          let (obj, v) = self.alloc(ObjKind::Object, rest.span.start);
          self.constrain(src, Constraint::SpreadInto(obj));
          self.bind_pattern(&rest.argument, v, kind);
        }
      }
      BindingPattern::ArrayPattern(p) => {
        let elem = self.fresh();
        self.constrain(src, Constraint::ElemOf(elem));
        for e in p.elements.iter().flatten() {
          if self.checkpoint(e.span().start) {
            break;
          }
          self.bind_pattern(e, elem, kind);
        }
        if let Some(rest) = &p.rest {
          let (arr, v) = self.alloc(ObjKind::Array, rest.span.start);
          let arr_elem = self.a.elem(arr);
          self.copy(elem, arr_elem);
          self.bind_pattern(&rest.argument, v, kind);
        }
      }
      BindingPattern::AssignmentPattern(p) => {
        let default = self.expr(&p.right);
        let v = self.fresh();
        self.copy(src, v);
        self.copy(default, v);
        self.bind_pattern(&p.left, v, kind);
      }
    }
  }

  fn property_key(&mut self, key: &PropertyKey<'_>, computed: bool) -> Key {
    match key {
      PropertyKey::StaticIdentifier(id) => Key::Name(ArcStr::from(id.name.as_str())),
      PropertyKey::PrivateIdentifier(id) => {
        Key::Name(ArcStr::from(format!("#{}", id.name.as_str())))
      }
      other => {
        let Some(expr) = other.as_expression() else { return Key::Unknown };
        if !computed {
          if let Some(s) = self.const_string(expr) {
            return Key::Name(ArcStr::from(s));
          }
        }
        self.key_of_expr(expr)
      }
    }
  }

  /// A property key from an expression: a constant string, else the value variable.
  fn key_of_expr(&mut self, expr: &Expression<'_>) -> Key {
    if let Some(s) = self.const_string(expr) {
      return Key::Name(ArcStr::from(s));
    }
    match expr {
      Expression::NumericLiteral(_)
      | Expression::BigIntLiteral(_)
      | Expression::BooleanLiteral(_)
      | Expression::NullLiteral(_) => Key::Unknown,
      _ => {
        let v = self.expr(expr);
        Key::Var(v)
      }
    }
  }

  fn const_string(&self, expr: &Expression<'_>) -> Option<String> {
    Self::const_string_of(expr)
  }

  fn const_string_of(expr: &Expression<'_>) -> Option<String> {
    match expr {
      Expression::StringLiteral(s) => Some(s.value.as_str()?.to_string()),
      Expression::TemplateLiteral(t) => {
        if !t.expressions.is_empty() {
          return None;
        }
        let mut out = String::new();
        for q in &t.quasis {
          out.push_str(q.value.cooked.as_ref()?.as_str()?);
        }
        Some(out)
      }
      Expression::BinaryExpression(b) if b.operator == BinaryOperator::Addition => {
        let l = Self::const_string_of(&b.left)?;
        let r = Self::const_string_of(&b.right)?;
        Some(l + &r)
      }
      Expression::ParenthesizedExpression(p) => Self::const_string_of(&p.expression),
      // `['a', 'b'].join('')`: a fresh literal of constant strings joined by a constant. A
      // patched `Array.prototype` bails the whole analysis (`store_name`), so the fold holds.
      Expression::CallExpression(call) if call.arguments.len() <= 1 => {
        let Expression::StaticMemberExpression(m) = &call.callee else { return None };
        let Expression::ArrayExpression(arr) = &m.object else { return None };
        if m.property.name.as_str() != "join" || call.optional || m.optional {
          return None;
        }
        let sep = match call.arguments.first() {
          None => ",".to_string(),
          Some(arg) => Self::const_string_of(arg.as_expression()?)?,
        };
        let parts = arr
          .elements
          .iter()
          .map(|e| Self::const_string_of(e.as_expression()?))
          .collect::<Option<Vec<String>>>()?;
        Some(parts.join(&sep))
      }
      _ => None,
    }
  }

  // == functions and classes

  /// `function () { return pipeArguments(this, arguments) }`, with `pipeArguments` bound to
  /// `Pipeable.pipeArguments`, is the `pipe` method of `Pipeable.Prototype`: the
  /// `effect:Pipeable.pipe` builtin, which applies its arguments in order to the receiver at each
  /// call site. As a closure it would collect every receiver and every piped function once its
  /// clones run out, and `pipeArguments` would apply each function to all of them.
  fn pipe_forwarder(&mut self, f: &Function<'_>) -> Option<VarId> {
    if f.r#async || f.generator || !f.params.items.is_empty() || f.params.rest.is_some() {
      return None;
    }
    let body = f.body.as_ref()?;
    if !body.directives.is_empty() {
      return None;
    }
    let [Statement::ReturnStatement(r)] = body.statements.as_slice() else { return None };
    let Some(Expression::CallExpression(c)) = &r.argument else { return None };
    if c.optional {
      return None;
    }
    let Expression::Identifier(callee) = &c.callee else { return None };
    let [Argument::ThisExpression(_), Argument::Identifier(args)] = c.arguments.as_slice() else {
      return None;
    };
    // `arguments` is the function's own arguments object, not a binding of that name.
    let args_symbol =
      args.reference_id.get().and_then(|id| self.m.scoping.get_reference(id).symbol_id());
    if args.name.as_str() != "arguments" || args_symbol.is_some() {
      return None;
    }
    let symbol =
      callee.reference_id.get().and_then(|id| self.m.scoping.get_reference(id).symbol_id())?;
    let canonical = self.sh.symbols.canonical_ref_for((self.m.module, symbol).into());
    let bound = *self.sh.rule_symbols.get(&canonical)?;
    let ObjKind::Builtin { rule, .. } = self.a.objs[bound].kind else { return None };
    if self.a.rule_id("effect:Pipeable.pipeArguments") != Some(rule) {
      return None;
    }
    let pipe = self.a.rule_id("effect:Pipeable.pipe")?;
    let method = self.a.builtin(pipe, None, None);
    let v = self.fresh();
    self.seed(v, method);
    // A named function expression's own name binding.
    if let Some(name) = &f.id {
      if let Some(bv) = self.binding_var(name) {
        self.copy(v, bv);
      }
    }
    Some(v)
  }

  fn function(&mut self, f: &Function<'_>, kind_hint: FnKind) -> VarId {
    if let Some(v) = self.pipe_forwarder(f) {
      return v;
    }
    let kind = match (f.r#async, f.generator) {
      (true, true) => FnKind::AsyncGenerator,
      (true, false) => FnKind::Async,
      (false, true) => FnKind::Generator,
      (false, false) => kind_hint,
    };
    let id = Self::new_fn(self.a, self.m.module, f.span.start, kind);
    let (obj, v) = self.alloc(ObjKind::Closure(id), f.span.start);
    self.a.fns[id].closure = Some(obj);
    // The function's own name binding, for named function expressions.
    if let Some(name) = &f.id {
      if let Some(bv) = self.binding_var(name) {
        self.install(v, bv);
      }
    }
    let outer = self.cur;
    self.a.fns[outer].leaf = false;
    self.cur = id;
    let this = self.a.fns[id].this;
    self.this_stack.push(this);
    self.generator_stack.push(f.generator);
    self.params(&f.params);
    if let Some(body) = &f.body {
      self.statements(&body.statements);
      self.a.fns[id].returns_closure_only = !f.generator
        && !f.r#async
        && matches!(body.statements.as_slice(), [Statement::ReturnStatement(r)] if matches!(r.argument, Some(Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_))));
    }
    self.generator_stack.pop();
    self.this_stack.pop();
    self.mark_cloneable(id);
    self.cur = outer;
    v
  }

  /// See `Analysis::clone_for`.
  fn mark_cloneable(&mut self, id: FnId) {
    const MAX_CLONE_PENDING: usize = 80;
    let info = &mut self.a.fns[id];
    info.cloneable = info.leaf
      && matches!(info.kind, FnKind::Normal | FnKind::Arrow)
      && info.pending.len() <= MAX_CLONE_PENDING;
  }

  fn arrow(&mut self, f: &ArrowFunctionExpression<'_>) -> VarId {
    let kind = if f.r#async { FnKind::Async } else { FnKind::Arrow };
    let id = Self::new_fn(self.a, self.m.module, f.span.start, kind);
    let (obj, v) = self.alloc(ObjKind::Closure(id), f.span.start);
    self.a.fns[id].closure = Some(obj);
    // Arrows share `this` with the enclosing function.
    let enclosing_this = *self.this_stack.last().unwrap();
    let this = self.a.fns[id].this;
    self.install(enclosing_this, this);
    self.a.fns[id].enclosing_this = Some(enclosing_this);
    let outer = self.cur;
    self.a.fns[outer].leaf = false;
    self.cur = id;
    self.generator_stack.push(false);
    self.params(&f.params);
    match &f.body {
      ArrowFunctionBody::FunctionBody(b) => {
        self.statements(&b.statements);
        self.a.fns[id].returns_closure_only = !f.r#async
          && matches!(b.statements.as_slice(), [Statement::ReturnStatement(r)] if matches!(r.argument, Some(Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_))));
      }
      other => {
        if let Some(e) = other.as_expression() {
          let r = self.expr(e);
          let ret = self.a.fns[id].ret;
          self.copy(r, ret);
          self.a.fns[id].returns_closure_only = !f.r#async
            && matches!(
              e,
              Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
            );
        }
      }
    }
    self.generator_stack.pop();
    self.mark_cloneable(id);
    self.cur = outer;
    v
  }

  fn params(&mut self, params: &FormalParameters<'_>) {
    let saved = self.tdz_until.replace(params.span.end);
    let mut vars = vec![];
    for p in &params.items {
      if self.checkpoint(p.span.start) {
        break;
      }
      let v = self.fresh();
      vars.push(v);
      let bound = if let Some(init) = &p.initializer {
        let d = self.expr(init);
        let u = self.fresh();
        self.copy(v, u);
        self.copy(d, u);
        u
      } else {
        v
      };
      self.bind_pattern(&p.pattern, bound, DeclKind::Param);
    }
    self.a.fns[self.cur].params = vars;
    if let Some(rest) = &params.rest {
      let v = self.fresh();
      self.a.fns[self.cur].rest = Some(v);
      self.bind_pattern(&rest.rest.argument, v, DeclKind::Param);
    }
    self.tdz_until = saved;
  }

  /// Whether a class's heritage is directly a key constructor: a call of effect's own
  /// `Context.Service`/`Context.Reference` (`Context.Service<..>()("id")`), or a binding a `const`
  /// declares as one. Both are resolved by symbol, never by spelling. Such a key class is
  /// generated; any other class, including a subclass of a key class, is a hole (`class_hole`).
  /// The binding counts when the generator has met its declaration first, as in program order:
  /// declared by an earlier top-level statement, or earlier in this one. A deferred statement
  /// is generated after statements that follow it, which must not change its classes.
  fn key_heritage(&self, c: &Class<'_>) -> bool {
    let Some(h) = &c.heritage else { return false };
    match &h.expression {
      Expression::Identifier(id) => self.canonical_of(id).is_some_and(|s| {
        self.sh.key_symbols.get(&s).is_some_and(|at| *at < self.seq || self.stmt_keys.contains(&s))
      }),
      e => self.key_constructor_call(e),
    }
  }

  /// `e` calls effect's own `Context.Service`/`Context.Reference`, in one of the forms
  /// `effect::key_declaration` accepts, with the callee resolved by symbol: an import of the
  /// constructor, or a member of a namespace import of a module exporting it.
  fn key_constructor_call(&self, e: &Expression<'_>) -> bool {
    if super::effect::key_declaration(e).is_none() {
      return false;
    }
    let mut callee = e;
    while let Expression::CallExpression(call) = callee {
      callee = &call.callee;
    }
    match callee {
      Expression::Identifier(id) => {
        self.canonical_of(id).is_some_and(|s| self.sh.key_ctor_symbols.contains(&s))
      }
      Expression::StaticMemberExpression(m) => {
        let Expression::Identifier(ns) = &m.object else { return false };
        let Some(s) = self.canonical_of(ns) else { return false };
        let Some(module) = self.sh.namespace_syms.get(&s) else { return false };
        ["Service", "Reference"].iter().any(|n| {
          *n == m.property.name.as_str() && self.sh.key_ctor_members.contains(&(*module, *n))
        })
      }
      _ => false,
    }
  }

  fn canonical_of(&self, id: &IdentifierReference<'_>) -> Option<SymbolRef> {
    let symbol = id.reference_id.get().and_then(|r| self.m.scoping.get_reference(r).symbol_id())?;
    Some(self.sh.symbols.canonical_ref_for((self.m.module, symbol).into()))
  }

  /// A class that is not a key class is a hole (Tim's class rule: a service that reaches class
  /// code bails). Its body is not generated. The class and its prototype are objects, with the
  /// prototype linked to the heritage's, so `instanceof`, `Object.create(C.prototype)` and
  /// heirs work; everything that crosses into them, or that the body references from outside,
  /// crosses into class code (`Analysis::class_escape`), and everything that comes out is the
  /// class's opaque value.
  fn class_hole(&mut self, c: &Class<'_>) -> VarId {
    self.a.fns[self.cur].leaf = false;
    let heritage = c.heritage.as_ref().map(|h| self.expr(&h.expression));
    let (proto_obj, prototype) = self.alloc(ObjKind::Object, c.span.start);
    if let Some(h) = heritage {
      let parent_proto = self.load(h, Key::Name(ArcStr::from("prototype")));
      let pv = self.a.proto_var(proto_obj);
      self.copy(parent_proto, pv);
    }
    let class_obj = self
      .a
      .alloc(ObjKind::Class { ctor: None, heritage, prototype }, Some(self.site(c.span.start)));
    let cv = self.fresh();
    self.seed(cv, class_obj);
    self.store(prototype, Key::Name(ArcStr::from("constructor")), cv);
    if let Some(h) = heritage {
      let pv = self.a.proto_var(class_obj);
      self.copy(h, pv);
    }
    if let Some(id) = &c.id {
      if let Some(bv) = self.binding_var(id) {
        self.install(cv, bv);
      }
    }
    self.a.hole(class_obj, proto_obj);
    // What the body references from outside crosses into it: a namespace member by member.
    let mut refs = FreeRefs {
      scoping: self.m.scoping,
      class_scope: c.scope_id.get(),
      refs: vec![],
      unsafe_lookup: false,
    };
    oxc::ast_visit::Visit::visit_class_body(&mut refs, &c.body);
    if refs.unsafe_lookup {
      self.a.bail_everything(
        "a class body references Context.getOrUndefinedUnsafe, which reaches a service by its key string",
      );
    }
    for (reference, name, start, path) in refs.refs {
      if self.checkpoint(start) {
        break;
      }
      let v = self.reference_parts(reference, &name, start);
      self.constrain(v, Constraint::ClassRef(class_obj, path));
    }
    cv
  }

  fn class(&mut self, c: &Class<'_>) -> VarId {
    if !self.key_heritage(c) {
      return self.class_hole(c);
    }
    self.a.fns[self.cur].leaf = false;
    let heritage = c.heritage.as_ref().map(|h| self.expr(&h.expression));
    let (proto_obj, prototype) = self.alloc(ObjKind::Object, c.span.start);
    if let Some(h) = heritage {
      let parent_proto = self.load(h, Key::Name(ArcStr::from("prototype")));
      let pv = self.a.proto_var(proto_obj);
      self.copy(parent_proto, pv);
    }
    let class_obj = self
      .a
      .alloc(ObjKind::Class { ctor: None, heritage, prototype }, Some(self.site(c.span.start)));
    let cv = self.fresh();
    self.seed(cv, class_obj);
    // `C.prototype.constructor` is `C`.
    self.store(prototype, Key::Name(ArcStr::from("constructor")), cv);
    if let Some(h) = heritage {
      let pv = self.a.proto_var(class_obj);
      self.copy(h, pv);
    }
    if let Some(id) = &c.id {
      if let Some(bv) = self.binding_var(id) {
        self.install(cv, bv);
      }
    }
    self.class_stack.push(heritage);
    for element in &c.body.body {
      if self.checkpoint(element.span().start) {
        break;
      }
      match element {
        ClassElement::MethodDefinition(m) => {
          let fv = self.function(&m.value, FnKind::Normal);
          let Some(fid) = self.closure_of(fv) else { continue };
          match m.kind {
            MethodDefinitionKind::Constructor => {
              if let ObjKind::Class { ctor, .. } = &mut self.a.objs[class_obj].kind {
                *ctor = Some(fid);
              }
            }
            MethodDefinitionKind::Method => {
              let target = if m.r#static { cv } else { prototype };
              let key = self.property_key(&m.key, m.computed);
              self.store(target, key, fv);
            }
            MethodDefinitionKind::Get => {
              // The getter runs on the target and on every object inheriting from it; its
              // result is the property's value there.
              let target = if m.r#static { cv } else { prototype };
              let key = self.property_key(&m.key, m.computed);
              let ret = self.a.fns[fid].ret;
              let this = self.a.fns[fid].this;
              self.copy(target, this);
              self.constrain(target, Constraint::Accessor(Accessor { this, setter: None }));
              self.emit(Pending::Activate(fid));
              self.store(target, key, ret);
            }
            MethodDefinitionKind::Set => {
              // The setter runs on the target and on every object inheriting from it, and
              // receives what is written under its name there.
              let target = if m.r#static { cv } else { prototype };
              let key = self.property_key(&m.key, m.computed);
              let this = self.a.fns[fid].this;
              self.copy(target, this);
              let param = self.a.fns[fid].params.first().copied();
              let setter = match (&key, param) {
                (Key::Name(name), Some(p)) => Some((name.clone(), p)),
                _ => None,
              };
              self.constrain(target, Constraint::Accessor(Accessor { this, setter }));
              self.emit(Pending::Activate(fid));
              let field = self.load(target, key);
              if let Some(p) = param {
                self.copy(field, p);
              }
            }
          }
        }
        ClassElement::PropertyDefinition(p) => {
          let target = if p.r#static { cv } else { prototype };
          let key = self.property_key(&p.key, p.computed);
          if let Some(value) = &p.value {
            let old_this = self.this_stack.last().copied();
            self.this_stack.push(if p.r#static { cv } else { prototype });
            let v = self.expr(value);
            self.this_stack.pop();
            let _ = old_this;
            self.store(target, key, v);
          }
        }
        ClassElement::AccessorProperty(p) => {
          let target = if p.r#static { cv } else { prototype };
          let key = self.property_key(&p.key, p.computed);
          if let Some(value) = &p.value {
            let v = self.expr(value);
            self.store(target, key, v);
          }
        }
        ClassElement::StaticBlock(b) => {
          self.this_stack.push(cv);
          self.statements(&b.body);
          self.this_stack.pop();
        }
        ClassElement::TSIndexSignature(_) => {}
      }
    }
    self.class_stack.pop();
    cv
  }

  // == expressions

  fn args(&mut self, args: &[Argument<'_>]) -> Vec<Arg> {
    args
      .iter()
      .map(|a| match a {
        Argument::SpreadElement(s) => Arg::Spread(self.expr(&s.argument)),
        other => Arg::Var(self.expr(other.to_expression())),
      })
      .collect()
  }

  fn call(
    &mut self,
    callee: VarId,
    this: Option<VarId>,
    args: Vec<Arg>,
    span: oxc::span::Span,
    is_new: bool,
  ) -> VarId {
    let ret = self.fresh();
    // Nested calls (`Context.Service()("key")`) share a start: the end tells them apart.
    let site = Site { module: self.m.module, start: span.start, sub: span.end | 0x8000_0000 };
    let call = Call { site, this, args, ret, is_new, builtin: None };
    self.constrain(callee, Constraint::Call(Rc::new(call)));
    ret
  }

  /// Callee and receiver of a call expression.
  fn callee(&mut self, callee: &Expression<'_>) -> (VarId, Option<VarId>) {
    match callee {
      Expression::StaticMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        let f = self.load(obj, Key::Name(ArcStr::from(m.property.name.as_str())));
        (f, Some(self.receiver(&m.object, obj)))
      }
      Expression::ComputedMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        let key = self.key_of_expr(&m.expression);
        let f = self.load(obj, key);
        (f, Some(self.receiver(&m.object, obj)))
      }
      Expression::PrivateFieldExpression(m) => {
        let obj = self.member_object(&m.object);
        let f = self.load(obj, Key::Name(ArcStr::from(format!("#{}", m.field.name.as_str()))));
        (f, Some(self.receiver(&m.object, obj)))
      }
      Expression::ParenthesizedExpression(p) => self.callee(&p.expression),
      Expression::ChainExpression(c) => match &c.expression {
        ChainElement::StaticMemberExpression(m) => {
          let obj = self.member_object(&m.object);
          let f = self.load(obj, Key::Name(ArcStr::from(m.property.name.as_str())));
          (f, Some(self.receiver(&m.object, obj)))
        }
        ChainElement::ComputedMemberExpression(m) => {
          let obj = self.member_object(&m.object);
          let key = self.key_of_expr(&m.expression);
          let f = self.load(obj, key);
          (f, Some(self.receiver(&m.object, obj)))
        }
        ChainElement::PrivateFieldExpression(m) => {
          let obj = self.member_object(&m.object);
          let f = self.load(obj, Key::Name(ArcStr::from(format!("#{}", m.field.name.as_str()))));
          (f, Some(self.receiver(&m.object, obj)))
        }
        ChainElement::CallExpression(c) => (self.call_expression(c), None),
        ChainElement::TSNonNullExpression(e) => self.callee(&e.expression),
      },
      other => (self.expr(other), None),
    }
  }

  /// The `this` a method call on `object` passes: `super.m()` looks `m` up on the parent
  /// prototype but calls it with the current `this`.
  fn receiver(&self, object: &Expression<'_>, looked_up_on: VarId) -> VarId {
    if matches!(object, Expression::Super(_)) {
      *self.this_stack.last().unwrap()
    } else {
      looked_up_on
    }
  }

  fn member_object(&mut self, object: &Expression<'_>) -> VarId {
    match object {
      Expression::Super(s) => self.super_prototype(s.span.start),
      other => self.expr(other),
    }
  }

  /// What `super.x` looks `x` up on: the parent's prototype in instance code, the parent class
  /// itself in static code.
  /// What `super.x` reads from: the heritage (static members) and its prototype (instance
  /// members), of every enclosing class, since a nested class's computed keys see the outer
  /// `super`. Class code bails what reaches it (`Analysis::class_bail`), so this need not tell
  /// static from instance members or one class from another.
  /// Nested classes make the heritages as many as the input is deep: each checks the budget.
  fn super_prototype(&mut self, start: u32) -> VarId {
    let heritages: Vec<VarId> = self.class_stack.iter().flatten().copied().collect();
    if heritages.is_empty() {
      return self.sh.top_host;
    }
    let v = self.fresh();
    for h in heritages {
      if self.checkpoint(start) {
        break;
      }
      self.copy(h, v);
      let prototype = self.load(h, Key::Name(ArcStr::from("prototype")));
      self.copy(prototype, v);
    }
    v
  }

  fn call_expression(&mut self, c: &CallExpression<'_>) -> VarId {
    // Direct `eval` sees every binding of the module.
    if let Expression::Identifier(id) = &c.callee {
      if id.name.as_str() == "eval"
        && id.reference_id.get().and_then(|r| self.m.scoping.get_reference(r).symbol_id()).is_none()
      {
        let args = self.args(&c.arguments);
        for a in &args {
          let v = match a {
            Arg::Var(v) | Arg::Spread(v) => *v,
          };
          self.constrain(v, Constraint::Escape(Flavour::Host, None));
        }
        let symbols: Vec<SymbolRef> =
          self.m.scoping.symbol_ids().map(|s| (self.m.module, s).into()).collect();
        for sym in symbols {
          if self.checkpoint(c.span.start) {
            break;
          }
          let v = self.symbol_var(sym);
          self.constrain(v, Constraint::Escape(Flavour::Host, None));
        }
        return self.sh.top_host;
      }
    }
    if matches!(c.callee, Expression::Super(_)) {
      // `super(...)` runs the parent constructor on this constructor's `this`, with its own
      // arguments. Afterwards `this` is what the parent yielded: the same instance, or an object
      // a parent constructor returned in its place (see `Analysis::construct_on`).
      let args = self.args(&c.arguments);
      let this = *self.this_stack.last().unwrap();
      let Some(heritage) = self.class_stack.last().copied().flatten() else { return this };
      let ret = self.call(heritage, Some(this), args, c.span, true);
      self.copy(ret, this);
      return this;
    }
    let (callee, this) = self.callee(&c.callee);
    let args = self.args(&c.arguments);
    // `require` of a module hole gives the hole's value, so what is handed to it crosses in.
    let required = self
      .m
      .dynamic_imports
      .get(&c.node_id.get().index())
      .copied()
      .flatten()
      .and_then(|target| self.sh.hole_values.get(&target).copied());
    if let Some(value) = required {
      let v = self.call(callee, this, args, c.span, false);
      self.seed(v, value);
      return v;
    }
    if let Expression::StaticMemberExpression(m) = &c.callee {
      // `o.m(...)`: the name lets a call into unseen code hand over only the receivers that
      // can hold that callee (`Analysis::method_names`).
      let site = Site { module: self.m.module, start: c.span.start, sub: c.span.end | 0x8000_0000 };
      self.a.method_names.insert(site, ArcStr::from(m.property.name.as_str()));
    }
    self.call(callee, this, args, c.span, false)
  }

  #[expect(clippy::too_many_lines, reason = "one match over expression kinds")]
  pub fn expr(&mut self, expr: &Expression<'_>) -> VarId {
    if self.checkpoint(oxc::span::GetSpan::span(expr).start) {
      return self.sh.prim;
    }
    if matches!(expr, Expression::CallExpression(_)) {
      if let Some(s) = self.const_string(expr) {
        return self.str_var(&s);
      }
    }
    match expr {
      Expression::BooleanLiteral(_)
      | Expression::NullLiteral(_)
      | Expression::BigIntLiteral(_)
      | Expression::RegExpLiteral(_) => self.sh.prim,
      Expression::NumericLiteral(n) => {
        if n.value >= 0.0 && n.value <= 64.0 && n.value.fract() == 0.0 {
          #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked just above to be a small non-negative integer"
          )]
          let obj = self.a.num(n.value as u32);
          let v = self.fresh();
          self.seed(v, obj);
          v
        } else {
          self.sh.prim
        }
      }
      Expression::StringLiteral(s) => match s.value.as_str() {
        Some(text) => self.str_var(text),
        None => self.sh.top_host,
      },
      Expression::TemplateLiteral(t) => {
        if let Some(s) = self.const_string(expr) {
          return self.str_var(&s);
        }
        for e in &t.expressions {
          if self.stopped {
            break;
          }
          let v = self.expr(e);
          self.constrain(v, Constraint::Protocol(Protocol::Primitive));
        }
        self.run_time_string(expr)
      }
      Expression::Identifier(id) => self.reference_var(id),
      Expression::Super(s) => self.super_prototype(s.span.start),
      Expression::ArrayExpression(arr) => {
        let (obj, v) = self.alloc(ObjKind::Array, arr.span.start);
        let elem = self.a.elem(obj);
        for e in &arr.elements {
          if self.checkpoint(e.span().start) {
            break;
          }
          match e {
            ArrayExpressionElement::SpreadElement(s) => {
              let sv = self.expr(&s.argument);
              self.constrain(sv, Constraint::ElemOf(elem));
            }
            ArrayExpressionElement::Elision(_) => {}
            other => {
              let ev = self.expr(other.to_expression());
              self.copy(ev, elem);
            }
          }
        }
        v
      }
      Expression::ArrowFunctionExpression(f) => self.arrow(f),
      Expression::AssignmentExpression(a) => self.assignment(a),
      Expression::AwaitExpression(a) => {
        let v = self.expr(&a.argument);
        let dst = self.fresh();
        self.constrain(v, Constraint::AwaitOf(dst));
        dst
      }
      Expression::BinaryExpression(b) => {
        if b.operator == BinaryOperator::Addition {
          if let Some(s) = self.const_string(expr) {
            return self.str_var(&s);
          }
        }
        let l = self.expr(&b.left);
        let r = self.expr(&b.right);
        match b.operator {
          BinaryOperator::Instanceof => {
            // `x instanceof R` runs `R[Symbol.hasInstance](x)` (`Analysis::has_instance`).
            let site = self.site(b.span.start);
            self.constrain(r, Constraint::HasInstance(l, site));
            self.sh.prim
          }
          BinaryOperator::In => {
            // `k in o` reads `k` on `o`.
            let key = self.key_of_expr(&b.left);
            let _ = self.load(r, key);
            self.sh.prim
          }
          BinaryOperator::Addition
          | BinaryOperator::LessThan
          | BinaryOperator::LessEqualThan
          | BinaryOperator::GreaterThan
          | BinaryOperator::GreaterEqualThan
          | BinaryOperator::Equality
          | BinaryOperator::Inequality
          | BinaryOperator::Subtraction
          | BinaryOperator::Multiplication
          | BinaryOperator::Division => {
            self.constrain(l, Constraint::Protocol(Protocol::Primitive));
            self.constrain(r, Constraint::Protocol(Protocol::Primitive));
            if b.operator == BinaryOperator::Addition {
              self.run_time_string(expr)
            } else {
              self.sh.prim
            }
          }
          _ => self.sh.prim,
        }
      }
      Expression::CallExpression(c) => self.call_expression(c),
      Expression::ChainExpression(c) => match &c.expression {
        ChainElement::CallExpression(call) => self.call_expression(call),
        ChainElement::StaticMemberExpression(m) => {
          let obj = self.member_object(&m.object);
          self.load(obj, Key::Name(ArcStr::from(m.property.name.as_str())))
        }
        ChainElement::ComputedMemberExpression(m) => {
          let obj = self.member_object(&m.object);
          let key = self.key_of_expr(&m.expression);
          self.load(obj, key)
        }
        ChainElement::PrivateFieldExpression(m) => {
          let obj = self.member_object(&m.object);
          self.load(obj, Key::Name(ArcStr::from(format!("#{}", m.field.name.as_str()))))
        }
        ChainElement::TSNonNullExpression(e) => self.expr(&e.expression),
      },
      Expression::ClassExpression(c) => self.class(c),
      Expression::ConditionalExpression(c) => {
        self.expr(&c.test);
        let a = self.expr(&c.consequent);
        let b = self.expr(&c.alternate);
        let v = self.fresh();
        self.copy(a, v);
        self.copy(b, v);
        v
      }
      Expression::FunctionExpression(f) => self.function(f, FnKind::Normal),
      Expression::ImportExpression(i) => {
        let value = self.a.var();
        let (_, v) = self.alloc(ObjKind::Promise { value }, i.span.start);
        match self.m.dynamic_imports.get(&i.node_id.get().index()).copied().flatten() {
          Some(module) => {
            let ns = self.sh.namespace_objs[&module];
            self.seed(value, ns);
          }
          None => {
            let top = self.sh.top_ext;
            self.copy(top, value);
          }
        }
        if let Some(o) = &i.options {
          self.expr(o);
        }
        self.expr(&i.source);
        v
      }
      Expression::LogicalExpression(l) => {
        let a = self.expr(&l.left);
        let b = self.expr(&l.right);
        let v = self.fresh();
        self.copy(a, v);
        self.copy(b, v);
        v
      }
      Expression::NewExpression(n) => {
        let callee = self.expr(&n.callee);
        let args = self.args(&n.arguments);
        self.call(callee, None, args, n.span, true)
      }
      Expression::ObjectExpression(o) => self.object(o),
      Expression::ParenthesizedExpression(p) => self.expr(&p.expression),
      Expression::SequenceExpression(s) => {
        let mut last = self.sh.prim;
        for e in &s.expressions {
          if self.stopped {
            break;
          }
          last = self.expr(e);
        }
        last
      }
      Expression::TaggedTemplateExpression(t) => {
        let (callee, this) = self.callee(&t.tag);
        let (strings, sv) = self.alloc(ObjKind::Array, t.span.start);
        let elem = self.a.elem(strings);
        let top = self.sh.top_host;
        self.copy(top, elem);
        let mut args = vec![Arg::Var(sv)];
        for e in &t.quasi.expressions {
          if self.stopped {
            break;
          }
          args.push(Arg::Var(self.expr(e)));
        }
        self.call(callee, this, args, t.span, false)
      }
      Expression::ThisExpression(_) => *self.this_stack.last().unwrap(),
      Expression::UnaryExpression(u) => {
        let v = self.expr(&u.argument);
        if matches!(
          u.operator,
          oxc::syntax::operator::UnaryOperator::UnaryPlus
            | oxc::syntax::operator::UnaryOperator::UnaryNegation
        ) {
          self.constrain(v, Constraint::Protocol(Protocol::Primitive));
        }
        if u.operator == oxc::syntax::operator::UnaryOperator::Typeof {
          return self.sh.top_host;
        }
        self.sh.prim
      }
      Expression::UpdateExpression(u) => {
        if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &u.argument {
          let v = self.reference_var(id);
          self.constrain(v, Constraint::Protocol(Protocol::Primitive));
        } else if let Some(m) = u.argument.as_member_expression() {
          let obj = self.member_object(m.object());
          let _ = obj;
        }
        self.sh.prim
      }
      Expression::YieldExpression(y) => {
        let Some(arg) = &y.argument else { return self.sh.prim };
        let v = self.expr(arg);
        let yielded = self.a.fns[self.cur].yielded;
        if y.delegate {
          // Delegating yields the effect itself (to the fiber) or the iterable's elements.
          self.copy(v, yielded);
          self.constrain(v, Constraint::ElemOf(yielded));
          let dst = self.fresh();
          self.constrain(v, Constraint::SuccessOf(dst));
          let yf = self.a.fns[self.cur].yielded_fail;
          self.constrain(v, Constraint::FailureOf(yf));
          dst
        } else {
          self.copy(v, yielded);
          self.sh.prim
        }
      }
      Expression::PrivateInExpression(p) => {
        let o = self.expr(&p.right);
        let _ = self.load(o, Key::Name(ArcStr::from(format!("#{}", p.left.name.as_str()))));
        self.sh.prim
      }
      Expression::StaticMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        self.load(obj, Key::Name(ArcStr::from(m.property.name.as_str())))
      }
      Expression::ComputedMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        let key = self.key_of_expr(&m.expression);
        self.load(obj, key)
      }
      Expression::PrivateFieldExpression(m) => {
        let obj = self.member_object(&m.object);
        self.load(obj, Key::Name(ArcStr::from(format!("#{}", m.field.name.as_str()))))
      }
      Expression::JSXElement(_)
      | Expression::JSXFragment(_)
      | Expression::ImportMeta(_)
      | Expression::NewTarget(_)
      | Expression::V8IntrinsicExpression(_) => self.sh.top_host,
      Expression::TSAsExpression(e) => self.expr(&e.expression),
      Expression::TSSatisfiesExpression(e) => self.expr(&e.expression),
      Expression::TSTypeAssertion(e) => self.expr(&e.expression),
      Expression::TSNonNullExpression(e) => self.expr(&e.expression),
      Expression::TSInstantiationExpression(e) => self.expr(&e.expression),
    }
  }

  fn object(&mut self, o: &ObjectExpression<'_>) -> VarId {
    let (obj, v) = self.alloc(ObjKind::Object, o.span.start);
    let mut props = vec![];
    for p in &o.properties {
      if self.checkpoint(p.span().start) {
        break;
      }
      match p {
        ObjectPropertyKind::SpreadProperty(s) => {
          let sv = self.expr(&s.argument);
          self.constrain(sv, Constraint::SpreadInto(obj));
          props.push(PropInfo {
            span_start: s.span.start,
            name: None,
            value: sv,
            shape: Shape::Other,
            accessor: false,
            spread: true,
          });
        }
        ObjectPropertyKind::ObjectProperty(p) => {
          let key = self.property_key(&p.key, p.computed);
          let name = match &key {
            Key::Name(n) => Some(n.clone()),
            _ => None,
          };
          if name.as_deref() == Some("__proto__") && !p.computed && !p.shorthand {
            let pv = self.expr(&p.value);
            let proto = self.a.proto_var(obj);
            self.copy(pv, proto);
            props.push(PropInfo {
              span_start: p.span.start,
              name,
              value: pv,
              shape: Shape::Other,
              accessor: false,
              spread: false,
            });
            continue;
          }
          let value = self.expr(&p.value);
          match p.kind {
            PropertyKind::Init => {
              self.store(v, key, value);
            }
            PropertyKind::Get => {
              // The getter runs whenever the property is read; its result is the value.
              if let Some(fid) = self.closure_of(value) {
                let ret = self.a.fns[fid].ret;
                let this = self.a.fns[fid].this;
                self.copy(v, this);
                self.emit(Pending::Activate(fid));
                self.store(v, key, ret);
              }
            }
            PropertyKind::Set => {
              if let Some(fid) = self.closure_of(value) {
                let this = self.a.fns[fid].this;
                self.copy(v, this);
                self.emit(Pending::Activate(fid));
                let field = self.load(v, key);
                if let Some(param) = self.a.fns[fid].params.first().copied() {
                  self.copy(field, param);
                }
              }
            }
          }
          let shape = self.shape_of(&p.value, value);
          props.push(PropInfo {
            span_start: p.span.start,
            name,
            value,
            shape,
            accessor: !matches!(p.kind, PropertyKind::Init),
            spread: false,
          });
        }
      }
    }
    self.sh.literals.insert(
      obj,
      LiteralInfo {
        module: self.m.module,
        span_start: o.span.start,
        func: self.cur,
        case: self.case_stack.last().copied(),
        props,
      },
    );
    v
  }

  fn closure_of(&self, v: VarId) -> Option<FnId> {
    self.a.vars[v].pts.iter().find_map(|o| match self.a.objs[*o].kind {
      ObjKind::Closure(f) => Some(f),
      _ => None,
    })
  }

  fn shape_of(&mut self, expr: &Expression<'_>, value: VarId) -> Shape {
    match expr {
      Expression::FunctionExpression(_) | Expression::ArrowFunctionExpression(_) => {
        Shape::FnLiteral
      }
      Expression::Identifier(id) => {
        match id.reference_id.get().and_then(|r| self.m.scoping.get_reference(r).symbol_id()) {
          Some(symbol) => {
            let sym: SymbolRef = (self.m.module, symbol).into();
            Shape::Ident(self.sh.symbols.canonical_ref_for(sym))
          }
          None => Shape::Other,
        }
      }
      Expression::CallExpression(c) => {
        let simple_args = c.arguments.iter().all(|a| match a {
          Argument::SpreadElement(_) => false,
          other => matches!(
            other.to_expression(),
            Expression::FunctionExpression(_)
              | Expression::ArrowFunctionExpression(_)
              | Expression::StringLiteral(_)
              | Expression::NumericLiteral(_)
              | Expression::BooleanLiteral(_)
              | Expression::NullLiteral(_)
              | Expression::Identifier(_)
              | Expression::StaticMemberExpression(_)
          ),
        });
        // The callee variable: re-evaluate the callee expression's variable cheaply by
        // looking at what `value` came from is not possible; record the callee var instead.
        let (callee, _) = self.callee(&c.callee);
        let _ = value;
        Shape::Call { callee, simple_args }
      }
      Expression::ParenthesizedExpression(p) => self.shape_of(&p.expression, value),
      _ => Shape::Other,
    }
  }

  fn assignment(&mut self, a: &AssignmentExpression<'_>) -> VarId {
    let right = self.expr(&a.right);
    let value = match a.operator {
      AssignmentOperator::Assign => right,
      AssignmentOperator::LogicalAnd
      | AssignmentOperator::LogicalOr
      | AssignmentOperator::LogicalNullish => {
        // The target keeps its value or takes the right side.
        let current = self.target_value(&a.left);
        let v = self.fresh();
        self.copy(right, v);
        if let Some(c) = current {
          self.copy(c, v);
        }
        v
      }
      AssignmentOperator::Addition => {
        let current = self.target_value(&a.left);
        if let Some(c) = current {
          self.constrain(c, Constraint::Protocol(Protocol::Primitive));
        }
        self.constrain(right, Constraint::Protocol(Protocol::Primitive));
        self.sh.top_host
      }
      _ => self.sh.prim,
    };
    self.assign_target(&a.left, value);
    value
  }

  fn target_value(&mut self, target: &AssignmentTarget<'_>) -> Option<VarId> {
    match target {
      AssignmentTarget::AssignmentTargetIdentifier(id) => Some(self.reference_var(id)),
      AssignmentTarget::StaticMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        Some(self.load(obj, Key::Name(ArcStr::from(m.property.name.as_str()))))
      }
      AssignmentTarget::ComputedMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        let key = self.key_of_expr(&m.expression);
        Some(self.load(obj, key))
      }
      _ => None,
    }
  }

  fn assign_target(&mut self, target: &AssignmentTarget<'_>, value: VarId) {
    // Like `bind_pattern`, nested destructuring targets check at each level.
    if matches!(
      target,
      AssignmentTarget::ArrayAssignmentTarget(_) | AssignmentTarget::ObjectAssignmentTarget(_)
    ) && self.checkpoint(oxc::span::GetSpan::span(target).start)
    {
      return;
    }
    match target {
      AssignmentTarget::AssignmentTargetIdentifier(id) => {
        let v = self.reference_var(id);
        self.copy(value, v);
      }
      AssignmentTarget::StaticMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        self.store(obj, Key::Name(ArcStr::from(m.property.name.as_str())), value);
      }
      AssignmentTarget::ComputedMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        let key = self.key_of_expr(&m.expression);
        self.store(obj, key, value);
      }
      AssignmentTarget::PrivateFieldExpression(m) => {
        let obj = self.member_object(&m.object);
        self.store(obj, Key::Name(ArcStr::from(format!("#{}", m.field.name.as_str()))), value);
      }
      AssignmentTarget::ArrayAssignmentTarget(p) => {
        let elem = self.fresh();
        self.constrain(value, Constraint::ElemOf(elem));
        for e in p.elements.iter().flatten() {
          if self.checkpoint(e.span().start) {
            break;
          }
          self.assign_target_maybe_default(e, elem);
        }
        if let Some(rest) = &p.rest {
          let (arr, v) = self.alloc(ObjKind::Array, rest.span.start);
          let arr_elem = self.a.elem(arr);
          self.copy(elem, arr_elem);
          self.assign_target(&rest.target, v);
        }
      }
      AssignmentTarget::ObjectAssignmentTarget(p) => {
        for prop in &p.properties {
          if self.checkpoint(prop.span().start) {
            break;
          }
          match prop {
            AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(i) => {
              let v = self.load(value, Key::Name(ArcStr::from(i.binding.name.as_str())));
              let bv = self.reference_var(&i.binding);
              self.copy(v, bv);
              if let Some(init) = &i.init {
                let d = self.expr(init);
                self.copy(d, bv);
              }
            }
            AssignmentTargetProperty::AssignmentTargetPropertyProperty(pp) => {
              let key = self.property_key(&pp.name, pp.computed);
              let v = self.load(value, key);
              self.assign_target_maybe_default(&pp.binding, v);
            }
          }
        }
        if let Some(rest) = &p.rest {
          let (obj, v) = self.alloc(ObjKind::Object, rest.span.start);
          self.constrain(value, Constraint::SpreadInto(obj));
          self.assign_target(&rest.target, v);
        }
      }
      AssignmentTarget::TSAsExpression(_)
      | AssignmentTarget::TSSatisfiesExpression(_)
      | AssignmentTarget::TSNonNullExpression(_)
      | AssignmentTarget::TSTypeAssertion(_) => {}
    }
  }

  fn assign_target_maybe_default(
    &mut self,
    target: &AssignmentTargetMaybeDefault<'_>,
    value: VarId,
  ) {
    match target {
      AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
        let dv = self.expr(&d.init);
        let v = self.fresh();
        self.copy(value, v);
        self.copy(dv, v);
        self.assign_target(&d.binding, v);
      }
      other => {
        if let Some(t) = other.as_assignment_target() {
          self.assign_target(t, value);
        }
      }
    }
  }
}

fn decl_kind(kind: VariableDeclarationKind) -> DeclKind {
  match kind {
    VariableDeclarationKind::Var => DeclKind::Var,
    VariableDeclarationKind::Const => DeclKind::Const,
    VariableDeclarationKind::Let
    | VariableDeclarationKind::Using
    | VariableDeclarationKind::AwaitUsing => DeclKind::Let,
  }
}
/// A statement that cannot run other code, read a property or throw anything but its own
/// `throw`s and engine errors: literals, fresh literals and bindings only.
fn inert_statement(stmt: &Statement<'_>) -> bool {
  match stmt {
    Statement::ThrowStatement(s) => inert_expr(&s.argument),
    Statement::ExpressionStatement(s) => inert_expr(&s.expression),
    Statement::BlockStatement(b) => b.body.iter().all(inert_statement),
    Statement::EmptyStatement(_) => true,
    Statement::VariableDeclaration(d) => d.declarations.iter().all(|d| {
      matches!(d.id, BindingPattern::BindingIdentifier(_)) && d.init.as_ref().is_none_or(inert_expr)
    }),
    _ => false,
  }
}

fn inert_expr(expr: &Expression<'_>) -> bool {
  match expr {
    Expression::BooleanLiteral(_)
    | Expression::NullLiteral(_)
    | Expression::NumericLiteral(_)
    | Expression::BigIntLiteral(_)
    | Expression::StringLiteral(_)
    | Expression::Identifier(_)
    | Expression::FunctionExpression(_)
    | Expression::ArrowFunctionExpression(_) => true,
    Expression::TemplateLiteral(t) => t.expressions.is_empty(),
    Expression::ParenthesizedExpression(p) => inert_expr(&p.expression),
    Expression::ArrayExpression(a) => a.elements.iter().all(|e| match e {
      ArrayExpressionElement::SpreadElement(_) => false,
      ArrayExpressionElement::Elision(_) => true,
      other => inert_expr(other.to_expression()),
    }),
    Expression::ObjectExpression(o) => o.properties.iter().all(|p| match p {
      ObjectPropertyKind::ObjectProperty(p) => {
        !p.computed && p.key.name().is_some_and(|n| n != "__proto__") && inert_expr(&p.value)
      }
      ObjectPropertyKind::SpreadProperty(_) => false,
    }),
    _ => false,
  }
}

/// Whether a statement may reach the unsafe-lookup bailout of `class_hole`: it names
/// `getOrUndefinedUnsafe` anywhere, in any of the ways `FreeRefs` detects in a class body. A
/// statement for which this holds is generated up front, so the bailout is the generator's own.
fn may_bail(stmt: &Statement<'_>) -> bool {
  let mut names = NamesUnsafeLookup(false);
  oxc::ast_visit::Visit::visit_statement(&mut names, stmt);
  names.0
}

struct NamesUnsafeLookup(bool);

impl<'a> oxc::ast_visit::Visit<'a> for NamesUnsafeLookup {
  fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
    self.0 |= it.name.as_str() == UNSAFE_LOOKUP;
  }

  fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
    self.0 |= it.property.name.as_str() == UNSAFE_LOOKUP;
    oxc::ast_visit::walk::walk_static_member_expression(self, it);
  }

  fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
    self.0 |= matches!(&it.expression, Expression::StringLiteral(s) if s.value == UNSAFE_LOOKUP);
    oxc::ast_visit::walk::walk_computed_member_expression(self, it);
  }
}

const UNSAFE_LOOKUP: &str = "getOrUndefinedUnsafe";

/// The references a class body makes to bindings declared outside it (`Gen::class_hole`):
/// for each, its reference, name, position and the static member names read through it
/// (`ns.sub.x` is `ns` with `["sub", "x"]`). Whether a reference is free is decided by its
/// resolved symbol's scope, never by spelling: a parameter or local that shadows an outer name
/// is local. Unresolved names are globals, and free.
struct FreeRefs<'s> {
  scoping: &'s Scoping,
  class_scope: Option<oxc::syntax::scope::ScopeId>,
  refs: Vec<(Option<ReferenceId>, String, u32, Vec<ArcStr>)>,
  unsafe_lookup: bool,
}

impl FreeRefs<'_> {
  fn record(&mut self, id: &IdentifierReference<'_>, path: Vec<ArcStr>) {
    if id.name.as_str() == UNSAFE_LOOKUP {
      self.unsafe_lookup = true;
    }
    let reference = id.reference_id.get();
    let symbol = reference.and_then(|r| self.scoping.get_reference(r).symbol_id());
    if let (Some(symbol), Some(class_scope)) = (symbol, self.class_scope) {
      let scope = self.scoping.symbol_scope_id(symbol);
      if self.scoping.scope_ancestors(scope).any(|s| s == class_scope) {
        return;
      }
    }
    self.refs.push((reference, id.name.to_string(), id.span.start, path));
  }
}

impl<'a> oxc::ast_visit::Visit<'a> for FreeRefs<'_> {
  fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
    self.record(it, vec![]);
  }

  fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
    if it.property.name.as_str() == UNSAFE_LOOKUP {
      self.unsafe_lookup = true;
    }
    let mut path = vec![ArcStr::from(it.property.name.as_str())];
    let mut object = &it.object;
    while let Expression::StaticMemberExpression(m) = object {
      path.push(ArcStr::from(m.property.name.as_str()));
      object = &m.object;
    }
    if let Expression::Identifier(id) = object {
      path.reverse();
      self.record(id, path);
    } else {
      oxc::ast_visit::walk::walk_static_member_expression(self, it);
    }
  }

  fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
    if let Expression::StringLiteral(s) = &it.expression {
      if s.value == UNSAFE_LOOKUP {
        self.unsafe_lookup = true;
      }
    }
    oxc::ast_visit::walk::walk_computed_member_expression(self, it);
  }
}

#[cfg(test)]
mod tests {
  use oxc::allocator::Allocator;
  use oxc::parser::Parser;
  use oxc::semantic::{Scoping, SemanticBuilder};
  use oxc::span::SourceType;
  use oxc_index::IndexVec;
  use rolldown_common::{ModuleIdx, SymbolRefDb, SymbolRefDbForModule};
  use rustc_hash::{FxHashMap, FxHashSet};

  use super::{Gen, ModuleFacts, Shared};
  use crate::stages::link_stage::effect_service_shaking::heap::{Analysis, ObjKind};

  fn scoping(allocator: &Allocator, source: &str) -> Scoping {
    let program = Parser::new(allocator, source, SourceType::mjs()).parse().program;
    SemanticBuilder::new().build(allocator.alloc(program)).semantic.into_scoping()
  }

  /// What generating a module's top level did when the variable cap was `headroom` variables
  /// above what the analysis held when the walk began.
  struct Walked {
    /// Where generation stopped (its offset), if it did.
    stopped: Option<u32>,
    /// The names whose declarations were generated.
    declared: FxHashSet<String>,
    /// Closures allocated.
    closures: usize,
    /// Variables allocated: all of them, and those past the cap.
    used: usize,
    overshoot: usize,
    /// Objects allocated, and points-to and copy entries installed.
    objects: usize,
    entries: u64,
    /// Constraints left pending for when the functions are reached.
    pending: usize,
    /// The generator's context (enclosing functions, classes, `switch` cases, `try` blocks,
    /// binding being declared) is back at the top level.
    restored: bool,
  }

  /// Generates `source` as the only module, eagerly. `key` names a binding that counts as a
  /// key constructor declared by an earlier statement, so a class extending it is generated
  /// member by member (`Gen::class`) rather than as a hole.
  fn walk(source: &str, headroom: usize, key: Option<&str>) -> Walked {
    let module = ModuleIdx::from_raw_unchecked(0);
    let allocator = Allocator::default();
    let program =
      allocator.alloc(Parser::new(&allocator, source, SourceType::mjs()).parse().program);
    let scoping = SemanticBuilder::new().build(program).semantic.into_scoping();
    let db_scoping = self::scoping(&allocator, source);
    let root = db_scoping.root_scope_id();
    let mut symbols = SymbolRefDb::new();
    symbols.store_local_db(module, SymbolRefDbForModule::new(db_scoping, module, root));
    let mut a = Analysis::new();
    let [prim, top_host, top_ext, thrown] = std::array::from_fn(|_| a.var());
    a.seed(prim, a.prim);
    a.seed(top_host, a.top_host);
    a.seed(top_ext, a.top_ext);
    let mut sh = Shared {
      symbols: &symbols,
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
      module_fns: IndexVec::from_vec(vec![None]),
      cjs_modules: FxHashSet::default(),
      lazy_decls: FxHashMap::default(),
      lazy_refs: vec![],
      lazy_by_module: FxHashMap::default(),
      resolve_global: |a, _, _| a.var(),
      key_symbols: FxHashMap::default(),
      statements: 1,
      lazy_spans: vec![],
      kept_eager: 0,
      scan_time: std::time::Duration::ZERO,
      key_ctor_symbols: FxHashSet::default(),
      key_ctor_members: FxHashSet::default(),
      hole_values: FxHashMap::default(),
    };
    let name_of = |name: &str| scoping.symbol_ids().find(|s| scoping.symbol_name(*s) == name);
    if let Some(key) = key {
      let sym = name_of(key).expect("the key binding");
      sh.key_symbols.insert((module, sym).into(), 0);
    }
    let facts = ModuleFacts {
      module,
      scoping: &scoping,
      external_imports: FxHashMap::default(),
      cjs_imports: FxHashSet::default(),
      dynamic_imports: FxHashMap::default(),
      default_export: (module, scoping.symbol_ids().next().expect("a symbol")).into(),
      lazy_stmts: FxHashMap::default(),
    };
    let mut g = Gen::new(&mut a, &mut sh, &facts);
    let start = g.a.vars.len();
    let objects = g.a.objs.len();
    let entries = g.a.total_pts + g.a.total_copies;
    let cap = start + headroom;
    g.a.set_var_cap(cap);
    g.program(program, false);
    let restored = g.cur == g.module_fn
      && g.this_stack.len() == 1
      && g.class_stack.is_empty()
      && g.generator_stack.len() == 1
      && g.tdz_until.is_none()
      && g.case_stack.is_empty()
      && g.try_throws.is_empty()
      && g.lazy_owner.is_none();
    drop(g);
    let (used, overshoot) = (a.vars.len() - start, a.vars.len().saturating_sub(cap));
    let declared = sh.decls.keys().map(|s| scoping.symbol_name(s.symbol).to_string()).collect();
    let closures = a.objs.iter().filter(|o| matches!(o.kind, ObjKind::Closure(_))).count();
    Walked {
      stopped: a.generation_stopped.map(|s| s.start),
      declared,
      closures,
      used,
      overshoot,
      objects: a.objs.len() - objects,
      entries: a.total_pts + a.total_copies - entries,
      pending: a.fns.iter().map(|f| f.pending.len()).sum(),
      restored,
    }
  }

  /// `n` comma-separated names, `prefix0` to `prefix{n-1}`.
  fn names(prefix: &str, n: usize) -> String {
    (0..n).map(|i| format!("{prefix}{i}")).collect::<Vec<_>>().join(", ")
  }

  /// Asserts that generation stopped part way through `source`, after `first` and before
  /// `last` (both searched for in it).
  fn stopped_between(walked: &Walked, source: &str, first: &str, last: &str) {
    let first = u32::try_from(source.find(first).expect("first")).unwrap();
    let last = u32::try_from(source.find(last).expect("last")).unwrap();
    let at = walked.stopped.expect("generation stopped");
    assert!(first < at && at < last, "stopped at {at}, not inside {first}..{last}");
  }

  /// Guard: walks that never enter an expression or a statement check the budget per child.
  /// A parameter list, a destructuring pattern and a key class's members, none with an
  /// expression in them, each take the analysis past a low variable cap: generation stops
  /// inside them, and the later siblings are never generated. Under a cap they stay within,
  /// they are generated whole.
  #[test]
  fn generation_stops_inside_expression_free_walks() {
    let n = 300;
    let params = format!("function wide({}) {{}}", names("p", n));
    let pattern = format!("function wide({{ {} }}) {{}}", names("d", n));
    let methods: Vec<String> = (0..n).map(|i| format!("m{i}() {{}}")).collect();
    let class = format!("const K = 0;\nclass C extends K {{ {} }}", methods.join(" "));
    for (source, prefix, key) in [(&params, "p", None), (&pattern, "d", None)] {
      let whole = walk(source, 100_000, key);
      assert_eq!(whole.stopped, None);
      assert!(whole.declared.contains(&format!("{prefix}{}", n - 1)));
      let walked = walk(source, 100, key);
      stopped_between(&walked, source, &format!("{prefix}1,"), &format!("{prefix}{}", n - 1));
      assert!(walked.declared.contains(&format!("{prefix}0")), "{source:.40}");
      assert!(!walked.declared.contains(&format!("{prefix}{}", n - 1)), "{source:.40}");
    }
    let whole = walk(&class, 100_000, Some("K"));
    assert_eq!((whole.stopped, whole.closures), (None, n));
    let walked = walk(&class, 100, Some("K"));
    stopped_between(&walked, &class, "m1()", &format!("m{}()", n - 1));
    assert!(walked.closures > 0 && walked.closures < n / 2, "{} closures", walked.closures);
  }

  /// Guard: recursive walks check the budget per level, not only per sibling. Rest elements
  /// nest with no sibling in between (`[...[...[...tail]]]`), in a binding and in an assignment;
  /// `super` in the innermost of nested key classes reads every enclosing heritage. Past a low
  /// variable cap generation stops inside the walk, at most a few variables over the cap, the
  /// deepest binding is never reached and the generator's context is restored.
  #[test]
  fn generation_stops_inside_recursive_walks() {
    let n = 100;
    let nested = format!("{}tail{}", "[...".repeat(n), "]".repeat(n));
    let binding = format!("function deep({nested}) {{}}");
    let assignment = format!("let tail;\n{nested} = [];");
    for source in [&binding, &assignment] {
      let whole = walk(source, 100_000, None);
      assert_eq!((whole.stopped, whole.restored), (None, true));
      assert!(whole.used > 2 * n, "{} variables", whole.used);
      let walked = walk(source, 50, None);
      stopped_between(&walked, source, "[...[", "tail]");
      assert!(walked.overshoot <= 3 && walked.restored, "{} over: {source:.30}", walked.overshoot);
      assert!(walked.used < whole.used / 2, "{} of {} variables", walked.used, whole.used);
    }
    assert!(!walk(&binding, 50, None).declared.contains("tail"));

    let class = |body: &str| {
      let open = (0..n).map(|i| format!("class C{i} extends K {{ m() {{ ")).collect::<Vec<_>>();
      let open = open.concat();
      format!("const K = 0;\n{open}{body}{}", "} } ".repeat(n))
    };
    let (without, with) = (class(""), class("super.x;"));
    let base = walk(&without, 100_000, Some("K")).used;
    let whole = walk(&with, 100_000, Some("K"));
    assert_eq!((whole.stopped, whole.restored), (None, true));
    assert!(whole.used >= base + n, "{} variables, {base} without super", whole.used);
    let walked = walk(&with, base + 10, Some("K"));
    let at = u32::try_from(with.find("super").unwrap()).unwrap();
    assert_eq!(walked.stopped, Some(at));
    // One heritage load crosses the cap; `.x`, the member read's own load, is the sink.
    assert!(walked.overshoot <= 2 && walked.restored, "{} over", walked.overshoot);
  }

  /// Guard: what a node does after its children return is bounded as well as what it does on
  /// the way down. Right-nested logical and conditional expressions and class heritages each
  /// allocate their own result once the child below returns, and a named class copies itself
  /// into its name, here already read in its heritage. With the cap crossed by the innermost
  /// node (an arrow), every node above still completes, and the next statement is where
  /// generation stops; or a child stops at its own check, and the nodes above complete after
  /// it. Either way they add nothing past the cap: the variables over it, the objects, the
  /// entries and the pending constraints are the same at two depths.
  #[test]
  fn generation_allocates_nothing_past_the_cap_while_unwinding() {
    let chain = |open: &str, leaf: &str, close: &str, n: usize| {
      format!("const v = {}{leaf}{};\nconst after = 0;", open.repeat(n), close.repeat(n))
    };
    // (open, leaf, close, variables allocated per level on the way down)
    let shapes = [
      ("null || (", "() => {}", ")", 0),
      ("null ? null : (", "() => {}", ")", 0),
      ("(class extends ", "(() => {})", " {})", 0),
      ("(class A extends (A, ", "() => {}", ") {})", 1),
      ("[] || (", "null", ")", 0),
    ];
    for (open, leaf, close, per_level) in shapes {
      let mut past = vec![];
      for n in [50, 100] {
        let source = chain(open, leaf, close, n);
        let whole = walk(&source, 100_000, None);
        assert_eq!((whole.stopped, whole.restored), (None, true));
        assert!(whole.used > n, "{} variables: {source:.30}", whole.used);
        let walked = if leaf == "null" {
          // Each `[]` allocates on the way down: the right operand at the cap stops.
          let walked = walk(&source, 50, None);
          stopped_between(&walked, &source, "[] || ([]", "null");
          walked
        } else {
          // The arrow's own variables cross the cap; nothing checks it until `after`.
          let walked = walk(&source, per_level * n + 3, None);
          let after = u32::try_from(source.find("const after").unwrap()).unwrap();
          assert_eq!(walked.stopped, Some(after), "{source:.30}");
          assert!(walked.objects <= 1, "{} objects: {source:.30}", walked.objects);
          walked
        };
        assert!(
          walked.overshoot <= 2 && walked.restored,
          "{} over: {source:.30}",
          walked.overshoot
        );
        past.push((walked.overshoot, walked.objects, walked.entries, walked.pending));
      }
      assert_eq!(past[0], past[1], "(over, objects, entries, pending) by depth: {open}");
    }
  }
}
