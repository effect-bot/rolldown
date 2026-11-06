//! Constraint generation: JavaScript semantics only.
//!
//! Every module's AST is walked once. Top-level code is installed immediately;
//! the constraints of a function body are kept pending on its `FnId` and
//! installed when the solver finds the function reachable. Nothing here knows
//! about Effect: symbols bound to a model rule (`rule_symbols`) are seeded with
//! the rule's builtin object and their initialisers are skipped.

#![expect(clippy::wildcard_imports, reason = "the oxc AST is used wholesale")]

use arcstr::ArcStr;
use oxc::ast::ast::*;
use oxc::semantic::Scoping;
use oxc::span::GetSpan;
use oxc::syntax::operator::{AssignmentOperator, BinaryOperator};
use oxc_index::IndexVec;
use rolldown_common::{ModuleIdx, SymbolRef, SymbolRefDb};
use rustc_hash::{FxHashMap, FxHashSet};

use super::heap::{
  Analysis, Arg, Call, Constraint, Flavour, FnId, FnInfo, FnKind, Key, ObjId, ObjKind, Pending,
  Protocol, Site, VarId,
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
  /// Resolves an unresolved global name to a variable, via the host table.
  pub resolve_global: fn(&mut Analysis, &mut FxHashMap<ArcStr, VarId>, &str) -> VarId,
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
  generator_stack: Vec<bool>,
  module_fn: FnId,
  /// End offset until which the binding being declared is uninitialised.
  tdz_until: Option<u32>,
  /// Innermost `switch` case ids.
  case_stack: Vec<u32>,
  next_case: u32,
}

impl<'a, 'l, 's> Gen<'a, 'l, 's> {
  pub fn new(a: &'a mut Analysis, sh: &'s mut Shared<'l>, m: &'s ModuleFacts<'l>) -> Self {
    let module_fn = Self::new_fn(a, m.module, 0, FnKind::Normal);
    sh.module_fns[m.module] = Some(module_fn);
    let this = a.fns[module_fn].this;
    let prim = sh.prim;
    a.copy(prim, this);
    Self {
      a,
      sh,
      m,
      cur: module_fn,
      this_stack: vec![this],
      class_stack: vec![],
      generator_stack: vec![false],
      module_fn,
      tdz_until: None,
      case_stack: vec![],
      next_case: 0,
    }
  }

  fn new_fn(a: &mut Analysis, module: ModuleIdx, span_start: u32, kind: FnKind) -> FnId {
    let this = a.var();
    let ret = a.var();
    let yielded_fail = a.var();
    let yielded = a.var();
    let arguments_elem = a.var();
    a.fns.push(FnInfo {
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
    })
  }

  pub fn module_fn(&self) -> FnId {
    self.module_fn
  }

  // == emission helpers

  fn site(&self, start: u32) -> Site {
    Site { module: self.m.module, start, sub: 0 }
  }

  fn emit(&mut self, p: Pending) {
    self.a.fns[self.cur].pending.push(p);
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

  fn fresh(&mut self) -> VarId {
    self.a.var()
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
    // its own.
    self.a.seed(v, obj);
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
    }
  }

  fn reference_var(&mut self, ident: &IdentifierReference<'_>) -> VarId {
    let symbol =
      ident.reference_id.get().and_then(|id| self.m.scoping.get_reference(id).symbol_id());
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
      None => {
        let name = ident.name.as_str();
        match name {
          "undefined" | "NaN" | "Infinity" => self.sh.prim,
          "arguments" => {
            if self.this_stack.len() > 1 {
              let elem = self.a.fns[self.cur].arguments_elem;
              let (arr, v) = self.alloc(ObjKind::Array, ident.span.start);
              let arr_elem = self.a.elem(arr);
              self.copy(elem, arr_elem);
              v
            } else {
              self.sh.top_host
            }
          }
          _ => (self.sh.resolve_global)(self.a, &mut self.sh.globals, name),
        }
      }
    }
  }

  // == program

  pub fn program(&mut self, program: &Program<'_>) {
    for stmt in &program.body {
      self.statement(stmt);
    }
  }

  fn statements(&mut self, stmts: &[Statement<'_>]) {
    for s in stmts {
      self.statement(s);
    }
  }

  fn statement(&mut self, stmt: &Statement<'_>) {
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
            self.bind_pattern(&param.pattern, v, DeclKind::Let);
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
      Statement::FunctionDeclaration(f) => {
        let v = self.function(f, FnKind::Normal);
        if let Some(id) = &f.id {
          self.record_decl(id, DeclKind::Function);
          if let Some(bv) = self.binding_var(id) {
            self.copy(v, bv);
          }
        }
      }
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
      Declaration::FunctionDeclaration(f) => {
        let v = self.function(f, FnKind::Normal);
        if let Some(id) = &f.id {
          self.record_decl(id, DeclKind::Function);
          if let Some(bv) = self.binding_var(id) {
            self.copy(v, bv);
          }
        }
      }
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

  fn variable_declaration(&mut self, d: &VariableDeclaration<'_>) {
    let kind = decl_kind(d.kind);
    for decl in &d.declarations {
      // A binding bound to a model rule keeps the rule and skips its initialiser.
      if let BindingPattern::BindingIdentifier(id) = &decl.id {
        if let Some(symbol) = id.symbol_id.get() {
          let sym: SymbolRef = (self.m.module, symbol).into();
          let canonical = self.sh.symbols.canonical_ref_for(sym);
          if self.sh.rule_symbols.contains_key(&canonical) {
            self.record_decl(id, kind);
            self.symbol_var(sym);
            continue;
          }
        }
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
    match pattern {
      BindingPattern::BindingIdentifier(id) => {
        self.record_decl(id, kind);
        if let Some(v) = self.binding_var(id) {
          self.copy(src, v);
        }
      }
      BindingPattern::ObjectPattern(p) => {
        for prop in &p.properties {
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
      _ => None,
    }
  }

  // == functions and classes

  fn function(&mut self, f: &Function<'_>, kind_hint: FnKind) -> VarId {
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
        self.a.copy(v, bv);
      }
    }
    let outer = self.cur;
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
    self.cur = outer;
    v
  }

  fn arrow(&mut self, f: &ArrowFunctionExpression<'_>) -> VarId {
    let kind = if f.r#async { FnKind::Async } else { FnKind::Arrow };
    let id = Self::new_fn(self.a, self.m.module, f.span.start, kind);
    let (obj, v) = self.alloc(ObjKind::Closure(id), f.span.start);
    self.a.fns[id].closure = Some(obj);
    // Arrows share `this` with the enclosing function.
    let enclosing_this = *self.this_stack.last().unwrap();
    let this = self.a.fns[id].this;
    self.a.copy(enclosing_this, this);
    let outer = self.cur;
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
    self.cur = outer;
    v
  }

  fn params(&mut self, params: &FormalParameters<'_>) {
    let saved = self.tdz_until.replace(params.span.end);
    let mut vars = vec![];
    for p in &params.items {
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

  fn class(&mut self, c: &Class<'_>) -> VarId {
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
    if let Some(h) = heritage {
      let pv = self.a.proto_var(class_obj);
      self.copy(h, pv);
    }
    if let Some(id) = &c.id {
      if let Some(bv) = self.binding_var(id) {
        self.a.copy(cv, bv);
      }
    }
    self.class_stack.push(heritage);
    for element in &c.body.body {
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
              let target = if m.r#static { cv } else { prototype };
              let key = self.property_key(&m.key, m.computed);
              let ret = self.a.fns[fid].ret;
              let this = self.a.fns[fid].this;
              self.copy(target, this);
              self.emit(Pending::Activate(fid));
              self.store(target, key, ret);
            }
            MethodDefinitionKind::Set => {
              let target = if m.r#static { cv } else { prototype };
              let key = self.property_key(&m.key, m.computed);
              let this = self.a.fns[fid].this;
              self.copy(target, this);
              self.emit(Pending::Activate(fid));
              let field = self.load(target, key);
              if let Some(p) = self.a.fns[fid].params.first().copied() {
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
    self.constrain(callee, Constraint::Call(Box::new(call)));
    ret
  }

  /// Callee and receiver of a call expression.
  fn callee(&mut self, callee: &Expression<'_>) -> (VarId, Option<VarId>) {
    match callee {
      Expression::StaticMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        let f = self.load(obj, Key::Name(ArcStr::from(m.property.name.as_str())));
        (f, Some(obj))
      }
      Expression::ComputedMemberExpression(m) => {
        let obj = self.member_object(&m.object);
        let key = self.key_of_expr(&m.expression);
        let f = self.load(obj, key);
        (f, Some(obj))
      }
      Expression::PrivateFieldExpression(m) => {
        let obj = self.member_object(&m.object);
        let f = self.load(obj, Key::Name(ArcStr::from(format!("#{}", m.field.name.as_str()))));
        (f, Some(obj))
      }
      Expression::ParenthesizedExpression(p) => self.callee(&p.expression),
      Expression::ChainExpression(c) => match &c.expression {
        ChainElement::StaticMemberExpression(m) => {
          let obj = self.member_object(&m.object);
          let f = self.load(obj, Key::Name(ArcStr::from(m.property.name.as_str())));
          (f, Some(obj))
        }
        ChainElement::ComputedMemberExpression(m) => {
          let obj = self.member_object(&m.object);
          let key = self.key_of_expr(&m.expression);
          let f = self.load(obj, key);
          (f, Some(obj))
        }
        ChainElement::PrivateFieldExpression(m) => {
          let obj = self.member_object(&m.object);
          let f = self.load(obj, Key::Name(ArcStr::from(format!("#{}", m.field.name.as_str()))));
          (f, Some(obj))
        }
        ChainElement::CallExpression(c) => (self.call_expression(c), None),
        ChainElement::TSNonNullExpression(e) => self.callee(&e.expression),
      },
      other => (self.expr(other), None),
    }
  }

  fn member_object(&mut self, object: &Expression<'_>) -> VarId {
    match object {
      Expression::Super(_) => self.super_prototype(),
      other => self.expr(other),
    }
  }

  fn super_prototype(&mut self) -> VarId {
    match self.class_stack.last().copied().flatten() {
      Some(h) => self.load(h, Key::Name(ArcStr::from("prototype"))),
      None => self.sh.top_host,
    }
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
          let v = self.symbol_var(sym);
          self.constrain(v, Constraint::Escape(Flavour::Host, None));
        }
        return self.sh.top_host;
      }
    }
    if matches!(c.callee, Expression::Super(_)) {
      // The parent constructor is invoked when the class is instantiated (see `heap.rs`).
      self.args(&c.arguments);
      return self.sh.prim;
    }
    let (callee, this) = self.callee(&c.callee);
    let args = self.args(&c.arguments);
    self.call(callee, this, args, c.span, false)
  }

  #[expect(clippy::too_many_lines, reason = "one match over expression kinds")]
  pub fn expr(&mut self, expr: &Expression<'_>) -> VarId {
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
          let v = self.expr(e);
          self.constrain(v, Constraint::Protocol(Protocol::Primitive));
        }
        self.sh.top_host
      }
      Expression::Identifier(id) => self.reference_var(id),
      Expression::Super(_) => self.super_prototype(),
      Expression::ArrayExpression(arr) => {
        let (obj, v) = self.alloc(ObjKind::Array, arr.span.start);
        let elem = self.a.elem(obj);
        for e in &arr.elements {
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
            if b.operator == BinaryOperator::Addition { self.sh.top_host } else { self.sh.prim }
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
