//! Type bounds from the `.d.ts` files the installed `effect` ships (Tim's decision on the
//! issue's second assumption: Effect's public types bound value flow).
//!
//! A minimal resolver, with no type checker. For each parameter and the return value of an
//! exported function it answers one question: which kinds of value can the declared type hold?
//! Primitives, functions, arrays, data-only records, objects that have certain members, or
//! anything (open). It follows imports and re-exports between `.d.ts` files, aliases, unions,
//! intersections (any one member is a superset), conditional types (both branches), interfaces
//! with their bases and classes. A position declared exactly `unknown` or `any` (or a rest
//! parameter, record or index signature of them) is ignored (Tim's decision on assumption 2): it
//! admits only primitives, and what reaches it is walked for its protocol methods instead. Type
//! parameters (never their constraints), `object`, mapped and indexed types, `typeof`, and
//! anything it cannot follow are open, so a gap in the resolver never drops a flow. See design
//! section 8.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use append_only_vec::AppendOnlyVec;
use oxc::allocator::Allocator;
use oxc::ast::ast::{
  BindingPattern, ClassElement, Declaration, Expression, FormalParameters,
  ImportDeclarationSpecifier, ModuleExportName, Program, Statement, TSSignature, TSType,
  TSTypeName, TSTypeOperatorOperator,
};
use oxc::parser::Parser;
use oxc::span::SourceType;
use rustc_hash::FxHashMap;
use self_cell::self_cell;

/// What a declared type admits. `members` lists alternatives: an object passes one if it has
/// every member named there (own fields or prototype chain). `ignored` marks an `unknown`/`any`
/// position: it admits nothing more, but what reaches it is walked for its protocol methods.
#[derive(Clone, Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per kind of value a type admits")]
pub struct Ty {
  pub open: bool,
  pub ignored: bool,
  pub prim: bool,
  pub func: bool,
  pub arrays: bool,
  pub any_object: bool,
  pub data_record: bool,
  pub members: Vec<Vec<String>>,
}

impl Ty {
  fn open() -> Self {
    Self { open: true, ..Self::default() }
  }

  fn prim() -> Self {
    Self { prim: true, ..Self::default() }
  }

  fn ignored() -> Self {
    Self { ignored: true, ..Self::default() }
  }

  /// `unknown`/`any` and nothing that admits an object.
  fn only_ignored(&self) -> bool {
    self.ignored && self.prim_only()
  }

  fn join(&mut self, o: Ty) {
    self.open |= o.open;
    self.ignored |= o.ignored;
    self.prim |= o.prim;
    self.func |= o.func;
    self.arrays |= o.arrays;
    self.any_object |= o.any_object;
    self.data_record |= o.data_record;
    self.members.extend(o.members);
  }

  /// Admits nothing but primitives.
  pub fn prim_only(&self) -> bool {
    !self.open
      && !self.func
      && !self.arrays
      && !self.any_object
      && !self.data_record
      && self.members.is_empty()
  }
}

/// The declared shape of what a function returns (`Dts::ret_shape`). `unknown` covers a
/// signature that could not be found or a type the resolver could not follow.
#[derive(Clone, Copy, Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per shape a return may take")]
pub struct RetShape {
  pub param: bool,
  pub effect: bool,
  pub effect_param: bool,
  pub effect_other: bool,
  pub prim: bool,
  pub other: bool,
  pub unknown: bool,
}

/// The bounds of one exported function: a type per parameter, the elements of its rest
/// parameter, and the return type (`None` is open).
#[derive(Clone, Debug, Default)]
pub struct Bounds {
  pub params: Vec<Option<Ty>>,
  pub rest: Option<Ty>,
  pub ret: Option<Ty>,
}

enum Local<'a> {
  Iface(Vec<&'a oxc::ast::ast::TSInterfaceDeclaration<'a>>),
  Alias(&'a oxc::ast::ast::TSTypeAliasDeclaration<'a>),
  Class(&'a oxc::ast::ast::Class<'a>),
  Value(Option<&'a TSType<'a>>),
  Funcs(Vec<&'a oxc::ast::ast::Function<'a>>),
  Import(String, String),
  ImportNs(String),
  Other,
}

/// One parsed `.d.ts`: its declarations by local name and its exports.
struct Index<'a> {
  dir: PathBuf,
  /// The file's name (`Tracer.d.ts`).
  file: String,
  locals: FxHashMap<String, Local<'a>>,
  /// Exported name to local name.
  exports: FxHashMap<String, String>,
  /// `export { a as b } from "./x"`: exported name to (specifier, imported name).
  reexports: FxHashMap<String, (String, String)>,
  stars: Vec<String>,
}

struct Owner {
  source: String,
  allocator: Allocator,
}

self_cell!(
  /// A parsed `.d.ts` file and its index, owning its source and arena.
  struct DtsFile {
    owner: Owner,

    #[covariant]
    dependent: Index,
  }
);

struct Sig<'a> {
  params: Vec<(Option<&'a TSType<'a>>, bool)>,
  ret: Option<&'a TSType<'a>>,
  tparams: Vec<String>,
  file: usize,
}

/// The `.d.ts` files read so far. Files are parsed when a bound first needs them, and live as
/// long as the analysis.
#[derive(Default)]
pub struct Dts {
  files: AppendOnlyVec<DtsFile>,
  by_path: RefCell<FxHashMap<PathBuf, Option<usize>>>,
  /// Time spent parsing and resolving, for the debug report.
  pub resolve_ns: Cell<u128>,
  pub files_parsed: Cell<usize>,
}

fn spec_path(dir: &Path, spec: &str) -> Option<PathBuf> {
  if !spec.starts_with('.') {
    return None;
  }
  let s = dir.join(spec).to_string_lossy().to_string();
  let base = s
    .strip_suffix(".d.ts")
    .or_else(|| s.strip_suffix(".ts"))
    .or_else(|| s.strip_suffix(".js"))
    .unwrap_or(&s);
  Some(PathBuf::from(format!("{base}.d.ts")))
}

/// The `unknown`/`any` rule covers Effect's public API only: in a module under `internal/`, an
/// ignored position is open.
fn internal_open(path: &Path, b: &mut Bounds) {
  if !path.components().any(|c| c.as_os_str() == "internal") {
    return;
  }
  let reopen = |t: &mut Option<Ty>| {
    if t.as_ref().is_some_and(|t| t.ignored) {
      *t = None;
    }
  };
  b.params.iter_mut().for_each(reopen);
  reopen(&mut b.rest);
  reopen(&mut b.ret);
}

fn export_name(n: &ModuleExportName<'_>) -> String {
  n.name().to_string()
}

impl Dts {
  /// The bounds of the function exported as `name` by the `.d.ts` at `path`, called with
  /// `nparams` parameters. Overloads with that many parameters are joined (all of them if none
  /// has), so a bound holds for every signature a call could match.
  pub fn bounds(&self, path: &Path, name: &str, nparams: usize) -> Bounds {
    let start = std::time::Instant::now();
    let mut out = self.bounds_inner(path, name, nparams);
    internal_open(path, &mut out);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  /// What a call to the function exported as `name` with `nparams` arguments returns, by
  /// declared shape (joined over the overloads that fit): a type parameter the caller picks, an
  /// `Effect` (and whether its success is a type parameter), a primitive, or anything else.
  pub fn ret_shape(&self, path: &Path, name: &str, nparams: usize) -> RetShape {
    let start = std::time::Instant::now();
    let mut out = RetShape::default();
    match self.signatures(path, name) {
      None => out.unknown = true,
      Some(sigs) => {
        let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
        let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
        for sig in matching {
          match sig.ret {
            Some(t) => self.ret_shape_of(sig.file, t, &sig.tparams, &mut out, 0),
            None => out.unknown = true,
          }
        }
      }
    }
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  fn ret_shape_of(&self, f: usize, t: &TSType<'_>, tp: &[String], out: &mut RetShape, depth: u32) {
    if depth > 8 {
      out.unknown = true;
      return;
    }
    match t {
      TSType::TSParenthesizedType(p) => {
        self.ret_shape_of(f, &p.type_annotation, tp, out, depth + 1);
      }
      TSType::TSUnionType(u) => {
        for m in &u.types {
          self.ret_shape_of(f, m, tp, out, depth + 1);
        }
      }
      TSType::TSTypeReference(r) => {
        if let TSTypeName::IdentifierReference(id) = &r.type_name {
          if tp.iter().any(|p| p == id.name.as_str()) {
            out.param = true;
            return;
          }
        }
        if let Some((rf, local)) = self.name_in(f, &r.type_name, 0) {
          let idx = self.index(rf);
          if idx.file == "Effect.d.ts" && local == "Effect" {
            out.effect = true;
            let success = r.type_arguments.as_ref().and_then(|a| a.params.first());
            match success {
              Some(TSType::TSTypeReference(sr)) if matches!(&sr.type_name, TSTypeName::IdentifierReference(id) if tp.iter().any(|p| p == id.name.as_str())) =>
              {
                out.effect_param = true;
              }
              Some(st) if self.classify(f, st, tp, depth + 1).prim_only() => {}
              _ => out.effect_other = true,
            }
            return;
          }
        }
        if self.classify(f, t, tp, depth + 1).prim_only() {
          out.prim = true;
        } else {
          out.other = true;
        }
      }
      _ => {
        let c = self.classify(f, t, tp, depth + 1);
        if c.prim_only() {
          out.prim = true;
        } else if c.open {
          out.unknown = true;
        } else {
          out.other = true;
        }
      }
    }
  }

  /// The parameter bounds shared by the callbacks the function exported as `name` takes: for
  /// each position, the join over every function-typed parameter of every signature. `None`
  /// when a parameter that is not a callback could still hold a function (an open or
  /// function-admitting type, or a rest parameter), or when nothing is narrowed.
  pub fn callback_bounds(&self, path: &Path, name: &str) -> Option<Vec<Option<Ty>>> {
    let start = std::time::Instant::now();
    let out = self.callback_bounds_inner(path, name);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  fn callback_bounds_inner(&self, path: &Path, name: &str) -> Option<Vec<Option<Ty>>> {
    let sigs = self.signatures(path, name)?;
    let mut acc: Vec<Option<Ty>> = vec![];
    let mut seen = 0usize;
    for sig in &sigs {
      for (t, rest) in &sig.params {
        let t = (*t)?;
        if *rest {
          return None;
        }
        let mut cbs = vec![];
        self.sigs_of_type(sig.file, t, &mut cbs, 0);
        if cbs.is_empty() {
          let ty = self.classify(sig.file, t, &sig.tparams, 0);
          if ty.open || ty.func {
            return None;
          }
          continue;
        }
        for cb in &mut cbs {
          cb.tparams.extend(sig.tparams.iter().cloned());
          for (k, (pt, prest)) in cb.params.iter().enumerate() {
            let ty = match pt {
              Some(pt) if !prest => self.classify(cb.file, pt, &cb.tparams, 0),
              _ => Ty::open(),
            };
            if acc.len() <= k {
              acc.resize(k + 1, None);
              // A position earlier callbacks lacked is open for them.
              if seen > 0 {
                acc[k] = Some(Ty::open());
              }
            }
            match &mut acc[k] {
              Some(x) => x.join(ty),
              None => acc[k] = Some(ty),
            }
          }
          // Positions this callback lacks receive nothing from it: no change.
          seen += 1;
        }
      }
    }
    let out: Vec<Option<Ty>> = acc.into_iter().map(|t| t.filter(|t| !t.open)).collect();
    if out.iter().all(Option::is_none) { None } else { Some(out) }
  }

  /// The bounds of the function that the function exported as `name` returns (`(input) => ...`
  /// in `decodeUnknownSync(schema)(input)`), which takes `nparams` parameters: the call
  /// signatures of each declared return type, with the outer type parameters still open.
  pub fn returned_bounds(&self, path: &Path, name: &str, nparams: usize) -> Bounds {
    let start = std::time::Instant::now();
    let mut inner = vec![];
    for sig in self.signatures(path, name).unwrap_or_default() {
      let Some(ret) = sig.ret else {
        inner.clear();
        break;
      };
      let before = inner.len();
      self.sigs_of_type(sig.file, ret, &mut inner, 0);
      if inner.len() == before {
        // A return type with no call signature to read: the returned function stays open.
        inner.clear();
        break;
      }
      for s in &mut inner[before..] {
        s.tparams.extend(sig.tparams.iter().cloned());
      }
    }
    let mut out = self.bounds_of_sigs(&inner, nparams);
    internal_open(path, &mut out);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  fn bounds_inner(&self, path: &Path, name: &str, nparams: usize) -> Bounds {
    let sigs = self.signatures(path, name).unwrap_or_default();
    self.bounds_of_sigs(&sigs, nparams)
  }

  fn bounds_of_sigs(&self, sigs: &[Sig<'_>], nparams: usize) -> Bounds {
    if sigs.is_empty() {
      return Bounds { params: vec![None; nparams], rest: None, ret: None };
    }
    let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
    let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
    let closed = |t: Ty| if t.open { None } else { Some(t) };
    let mut params = Vec::with_capacity(nparams);
    for i in 0..nparams {
      let mut acc: Option<Ty> = None;
      for sig in &matching {
        let t = match sig.params.get(i) {
          Some((Some(t), false)) => self.classify(sig.file, t, &sig.tparams, 0),
          // A missing parameter, a rest parameter or no annotation: open.
          _ => Ty::open(),
        };
        match &mut acc {
          Some(x) => x.join(t),
          None => acc = Some(t),
        }
      }
      params.push(acc.and_then(closed));
    }
    // A rest parameter's elements: a declared rest at the same position in every matching
    // signature, or open.
    let mut rest: Option<Ty> = None;
    for sig in &matching {
      let t = match sig.params.last() {
        Some((Some(t), true)) if sig.params.len() - 1 == nparams => {
          self.rest_element(sig.file, t, &sig.tparams)
        }
        _ => Ty::open(),
      };
      match &mut rest {
        Some(x) => x.join(t),
        None => rest = Some(t),
      }
    }
    let mut ret: Option<Ty> = None;
    for sig in &matching {
      let t = sig.ret.map_or_else(Ty::open, |t| self.classify(sig.file, t, &sig.tparams, 0));
      match &mut ret {
        Some(x) => x.join(t),
        None => ret = Some(t),
      }
    }
    Bounds { params, rest: rest.and_then(closed), ret: ret.and_then(closed) }
  }

  /// The element type of a rest parameter's array type, classified.
  fn rest_element(&self, f: usize, t: &TSType<'_>, tp: &[String]) -> Ty {
    match t {
      TSType::TSArrayType(a) => self.classify(f, &a.element_type, tp, 1),
      TSType::TSParenthesizedType(p) => self.rest_element(f, &p.type_annotation, tp),
      TSType::TSTypeOperatorType(o) if o.operator == TSTypeOperatorOperator::Readonly => {
        self.rest_element(f, &o.type_annotation, tp)
      }
      TSType::TSTypeReference(r) => match &r.type_name {
        TSTypeName::IdentifierReference(id)
          if matches!(id.name.as_str(), "Array" | "ReadonlyArray")
            && !self.index(f).locals.contains_key(id.name.as_str()) =>
        {
          r.type_arguments
            .as_ref()
            .and_then(|a| a.params.first())
            .map_or_else(Ty::open, |e| self.classify(f, e, tp, 1))
        }
        _ => Ty::open(),
      },
      _ => Ty::open(),
    }
  }

  fn index(&self, file: usize) -> &Index<'_> {
    self.files[file].borrow_dependent()
  }

  fn file(&self, path: &Path) -> Option<usize> {
    if let Some(f) = self.by_path.borrow().get(path) {
      return *f;
    }
    let parsed = std::fs::read_to_string(path).ok().map(|source| {
      let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
      let cell = DtsFile::new(Owner { source, allocator: Allocator::default() }, |owner| {
        let ret = Parser::new(&owner.allocator, &owner.source, SourceType::d_ts()).parse();
        let program: &Program<'_> = owner.allocator.alloc(ret.program);
        index(program, dir, path.file_name().map(|n| n.to_string_lossy().to_string()))
      });
      self.files_parsed.set(self.files_parsed.get() + 1);
      self.files.push(cell)
    });
    self.by_path.borrow_mut().insert(path.to_path_buf(), parsed);
    parsed
  }

  /// An exported name, as (file, local name).
  fn export(&self, path: &Path, name: &str, depth: u32) -> Option<(usize, String)> {
    if depth > 8 {
      return None;
    }
    let f = self.file(path)?;
    let idx = self.index(f);
    if let Some(local) = idx.exports.get(name) {
      return Some((f, local.clone()));
    }
    if let Some((spec, imported)) = idx.reexports.get(name) {
      let p = spec_path(&idx.dir, spec)?;
      return self.export(&p, imported, depth + 1);
    }
    for spec in &idx.stars {
      if let Some(p) = spec_path(&idx.dir, spec) {
        if let Some(r) = self.export(&p, name, depth + 1) {
          return Some(r);
        }
      }
    }
    None
  }

  /// The call signatures of an exported value.
  fn signatures(&self, path: &Path, name: &str) -> Option<Vec<Sig<'_>>> {
    let (f, local) = self.export(path, name, 0)?;
    let mut out = vec![];
    match self.index(f).locals.get(&local)? {
      Local::Funcs(fs) => {
        for func in fs {
          out.push(sig_of(
            &func.params,
            func.return_type.as_ref().map(|r| &r.type_annotation),
            func.type_parameters.as_deref(),
            f,
          ));
        }
      }
      Local::Value(Some(t)) => self.sigs_of_type(f, t, &mut out, 0),
      _ => return None,
    }
    if out.is_empty() { None } else { Some(out) }
  }

  fn sigs_of_type<'s>(&'s self, f: usize, t: &'s TSType<'s>, out: &mut Vec<Sig<'s>>, depth: u32) {
    if depth > 6 {
      return;
    }
    match t {
      TSType::TSFunctionType(ft) => out.push(sig_of(
        &ft.params,
        Some(&ft.return_type.type_annotation),
        ft.type_parameters.as_deref(),
        f,
      )),
      TSType::TSTypeLiteral(lit) => {
        for m in &lit.members {
          if let TSSignature::TSCallSignatureDeclaration(c) = m {
            out.push(sig_of(
              &c.params,
              c.return_type.as_ref().map(|r| &r.type_annotation),
              c.type_parameters.as_deref(),
              f,
            ));
          }
        }
      }
      TSType::TSParenthesizedType(p) => self.sigs_of_type(f, &p.type_annotation, out, depth + 1),
      TSType::TSTypeReference(r) => {
        if let Some((rf, local)) = self.name_in(f, &r.type_name, 0) {
          match self.index(rf).locals.get(&local) {
            Some(Local::Alias(a)) => self.sigs_of_type(rf, &a.type_annotation, out, depth + 1),
            Some(Local::Iface(is)) => {
              for i in is {
                for m in &i.body.body {
                  if let TSSignature::TSCallSignatureDeclaration(c) = m {
                    out.push(sig_of(
                      &c.params,
                      c.return_type.as_ref().map(|r| &r.type_annotation),
                      c.type_parameters.as_deref(),
                      rf,
                    ));
                  }
                }
              }
            }
            _ => {}
          }
        }
      }
      _ => {}
    }
  }

  /// A type name in `f`, as (file, local name), following imports.
  fn name_in(&self, f: usize, n: &TSTypeName<'_>, depth: u32) -> Option<(usize, String)> {
    if depth > 8 {
      return None;
    }
    match n {
      TSTypeName::IdentifierReference(id) => self.lookup(f, id.name.as_str(), depth),
      TSTypeName::QualifiedName(q) => {
        let TSTypeName::IdentifierReference(left) = &q.left else { return None };
        let idx = self.index(f);
        match idx.locals.get(left.name.as_str())? {
          Local::ImportNs(spec) => {
            let p = spec_path(&idx.dir, spec)?;
            self.export(&p, q.right.name.as_str(), depth + 1)
          }
          _ => None,
        }
      }
      TSTypeName::ThisExpression(_) => None,
    }
  }

  fn lookup(&self, f: usize, name: &str, depth: u32) -> Option<(usize, String)> {
    let idx = self.index(f);
    match idx.locals.get(name)? {
      Local::Import(spec, imported) => {
        let p = spec_path(&idx.dir, spec)?;
        self.export(&p, imported, depth + 1)
      }
      _ => Some((f, name.to_string())),
    }
  }

  fn classify(&self, f: usize, t: &TSType<'_>, tp: &[String], depth: u32) -> Ty {
    if depth > 14 {
      return Ty::open();
    }
    match t {
      TSType::TSStringKeyword(_)
      | TSType::TSNumberKeyword(_)
      | TSType::TSBooleanKeyword(_)
      | TSType::TSBigIntKeyword(_)
      | TSType::TSSymbolKeyword(_)
      | TSType::TSNullKeyword(_)
      | TSType::TSUndefinedKeyword(_)
      | TSType::TSVoidKeyword(_)
      | TSType::TSLiteralType(_)
      | TSType::TSTemplateLiteralType(_)
      | TSType::TSIntrinsicKeyword(_)
      | TSType::TSTypePredicate(_) => Ty::prim(),
      TSType::TSNeverKeyword(_) => Ty::default(),
      // Ignored positions (assumption 2): only a declared `unknown`/`any`. A type parameter is a
      // reference, and stays open whatever its constraint.
      TSType::TSUnknownKeyword(_) | TSType::TSAnyKeyword(_) => Ty::ignored(),
      TSType::TSFunctionType(_) | TSType::TSConstructorType(_) => {
        Ty { func: true, ..Ty::default() }
      }
      TSType::TSArrayType(_) | TSType::TSTupleType(_) => Ty { arrays: true, ..Ty::default() },
      TSType::TSParenthesizedType(p) => self.classify(f, &p.type_annotation, tp, depth + 1),
      TSType::TSTypeOperatorType(o) => match o.operator {
        TSTypeOperatorOperator::Keyof | TSTypeOperatorOperator::Unique => Ty::prim(),
        TSTypeOperatorOperator::Readonly => self.classify(f, &o.type_annotation, tp, depth + 1),
      },
      TSType::TSUnionType(u) => {
        let mut out = Ty::default();
        for m in &u.types {
          out.join(self.classify(f, m, tp, depth + 1));
        }
        out
      }
      TSType::TSIntersectionType(i) => {
        // Any member is a superset of the intersection: the first that is not open. `unknown`
        // is the identity of `&`, so it bounds nothing unless every member is ignored.
        let members: Vec<Ty> = i.types.iter().map(|m| self.classify(f, m, tp, depth + 1)).collect();
        if members.iter().all(Ty::only_ignored) {
          return Ty::ignored();
        }
        members.into_iter().find(|c| !c.open && !c.only_ignored()).unwrap_or_else(Ty::open)
      }
      TSType::TSConditionalType(c) => {
        // The result is one of the branches; `infer` names are open.
        let mut out = self.classify(f, &c.true_type, tp, depth + 1);
        out.join(self.classify(f, &c.false_type, tp, depth + 1));
        out
      }
      TSType::TSTypeLiteral(lit) => self.members_of(f, &lit.members, tp, depth),
      TSType::TSTypeReference(r) => self.reference(f, r, tp, depth),
      // `object`, `this`, mapped and indexed types, `typeof`, `import()`, `infer` and anything
      // else: open.
      _ => Ty::open(),
    }
  }

  fn members_of(&self, f: usize, ms: &[TSSignature<'_>], tp: &[String], depth: u32) -> Ty {
    let mut req = vec![];
    let mut out = Ty::default();
    let mut index_value: Option<Ty> = None;
    for m in ms {
      match m {
        TSSignature::TSPropertySignature(p) if !p.optional => {
          if let Some(n) = p.key.static_name() {
            req.push(n.display().to_string());
          }
        }
        TSSignature::TSMethodSignature(p) if !p.optional => {
          if let Some(n) = p.key.static_name() {
            req.push(n.display().to_string());
          }
        }
        TSSignature::TSCallSignatureDeclaration(_)
        | TSSignature::TSConstructSignatureDeclaration(_) => out.func = true,
        TSSignature::TSIndexSignature(ix) => {
          let v = self.classify(f, &ix.type_annotation.type_annotation, tp, depth + 1);
          index_value.get_or_insert_with(Ty::default).join(v);
        }
        _ => {}
      }
    }
    if let Some(v) = index_value {
      // `{ [k: string]: unknown }`: a record of ignored values is ignored.
      if v.only_ignored() && req.is_empty() && !out.func {
        return Ty::ignored();
      }
      if v.prim_only() {
        out.data_record = true;
      } else {
        out.any_object = true;
      }
    }
    if req.is_empty() {
      if !out.func && !out.data_record {
        out.any_object = true;
      }
    } else {
      out.members.push(req);
    }
    out
  }

  fn reference(
    &self,
    f: usize,
    r: &oxc::ast::ast::TSTypeReference<'_>,
    tp: &[String],
    depth: u32,
  ) -> Ty {
    if let TSTypeName::IdentifierReference(id) = &r.type_name {
      let n = id.name.as_str();
      if tp.iter().any(|p| p == n) {
        return Ty::open();
      }
      if !self.index(f).locals.contains_key(n) {
        let arg = |i: usize| r.type_arguments.as_ref().and_then(|a| a.params.get(i));
        return match n {
          "Array" | "ReadonlyArray" => Ty { arrays: true, ..Ty::default() },
          "Record" => match arg(1).map(|v| self.classify(f, v, tp, depth + 1)) {
            // `Record<string, unknown>`: a record of ignored values is ignored.
            Some(v) if v.only_ignored() => Ty::ignored(),
            Some(v) if v.prim_only() => Ty { data_record: true, ..Ty::default() },
            Some(_) => Ty { any_object: true, ..Ty::default() },
            None => Ty::open(),
          },
          "Readonly" | "NonNullable" => {
            arg(0).map_or_else(Ty::open, |a| self.classify(f, a, tp, depth + 1))
          }
          "Promise" | "PromiseLike" => Ty { members: vec![vec!["then".into()]], ..Ty::default() },
          "Function" => Ty { func: true, ..Ty::default() },
          "String" | "Number" | "Boolean" | "BigInt" | "Symbol" => Ty::prim(),
          "Partial" | "Required" | "Pick" | "Omit" | "Iterable" | "IterableIterator"
          | "AsyncIterable" | "Iterator" | "Map" | "ReadonlyMap" | "Set" | "ReadonlySet"
          | "WeakMap" | "WeakSet" | "Date" | "RegExp" | "Error" | "URL" | "URLSearchParams"
          | "Uint8Array" | "ArrayBuffer" | "Headers" | "Request" | "Response"
          | "ReadableStream" | "AbortSignal" | "Blob" | "FormData" | "File" => {
            Ty { any_object: true, ..Ty::default() }
          }
          _ => Ty::open(),
        };
      }
    }
    let Some((rf, local)) = self.name_in(f, &r.type_name, 0) else { return Ty::open() };
    let idx = self.index(rf);
    let public = idx.dir.file_name().is_some_and(|d| d == "dist");
    if public
      && super::effect::INERT_TYPES
        .iter()
        .any(|(file, names)| *file == idx.file && names.contains(&local.as_str()))
    {
      return Ty::ignored();
    }
    match self.index(rf).locals.get(&local) {
      Some(Local::Alias(a)) => {
        let mut tp2: Vec<String> = tp.to_vec();
        if let Some(d) = &a.type_parameters {
          tp2.extend(d.params.iter().map(|p| p.name.name.as_str().to_string()));
        }
        self.classify(rf, &a.type_annotation, &tp2, depth + 1)
      }
      Some(Local::Iface(is)) => self.interface(rf, is, depth),
      Some(Local::Class(c)) => self.class(rf, c, depth),
      _ => Ty::open(),
    }
  }

  fn interface(
    &self,
    f: usize,
    is: &[&oxc::ast::ast::TSInterfaceDeclaration<'_>],
    depth: u32,
  ) -> Ty {
    if depth > 14 {
      return Ty::open();
    }
    let mut req: Vec<String> = vec![];
    let mut out = Ty::default();
    for i in is {
      let tp: Vec<String> = i
        .type_parameters
        .as_ref()
        .map(|d| d.params.iter().map(|p| p.name.name.as_str().to_string()).collect())
        .unwrap_or_default();
      let own = self.members_of(f, &i.body.body, &tp, depth + 1);
      out.func |= own.func;
      out.data_record |= own.data_record;
      for m in own.members {
        req.extend(m);
      }
      // An interface has every required member of its bases.
      for h in &i.extends {
        if let Some((hf, local)) = self.name_in(f, &h.type_name, 0) {
          match self.index(hf).locals.get(&local) {
            Some(Local::Iface(bis)) => {
              let b = self.interface(hf, bis, depth + 1);
              out.func |= b.func;
              for m in b.members {
                req.extend(m);
              }
            }
            Some(Local::Class(c)) => {
              for m in self.class(hf, c, depth + 1).members {
                req.extend(m);
              }
            }
            _ => {}
          }
        }
      }
    }
    req.sort();
    req.dedup();
    if req.is_empty() {
      if !out.func && !out.data_record {
        out.any_object = true;
      }
    } else {
      out.members.push(req);
    }
    out
  }

  fn class(&self, f: usize, c: &oxc::ast::ast::Class<'_>, depth: u32) -> Ty {
    if depth > 14 {
      return Ty::open();
    }
    let mut req: Vec<String> = vec![];
    for e in &c.body.body {
      match e {
        ClassElement::PropertyDefinition(p) if !p.optional && !p.r#static => {
          if let Some(n) = p.key.static_name() {
            req.push(n.display().to_string());
          }
        }
        ClassElement::MethodDefinition(m) if !m.optional && !m.r#static => {
          if let Some(n) = m.key.static_name() {
            if n != "constructor" {
              req.push(n.display().to_string());
            }
          }
        }
        _ => {}
      }
    }
    if let Some(Expression::Identifier(id)) = c.heritage.as_ref().map(|h| &h.expression) {
      if let Some((hf, local)) = self.lookup(f, id.name.as_str(), 0) {
        if let Some(Local::Class(bc)) = self.index(hf).locals.get(&local) {
          for m in self.class(hf, bc, depth + 1).members {
            req.extend(m);
          }
        }
      }
    }
    req.sort();
    req.dedup();
    if req.is_empty() {
      Ty { any_object: true, ..Ty::default() }
    } else {
      Ty { members: vec![req], ..Ty::default() }
    }
  }
}

fn sig_of<'a>(
  params: &'a FormalParameters<'a>,
  ret: Option<&'a TSType<'a>>,
  tps: Option<&'a oxc::ast::ast::TSTypeParameterDeclaration<'a>>,
  file: usize,
) -> Sig<'a> {
  let mut ps: Vec<(Option<&'a TSType<'a>>, bool)> = params
    .items
    .iter()
    .map(|p| (p.type_annotation.as_ref().map(|a| &a.type_annotation), false))
    .collect();
  if let Some(r) = &params.rest {
    ps.push((r.type_annotation.as_ref().map(|a| &a.type_annotation), true));
  }
  let tparams = tps
    .map(|d| d.params.iter().map(|p| p.name.name.as_str().to_string()).collect())
    .unwrap_or_default();
  Sig { params: ps, ret, tparams, file }
}

fn index<'a>(program: &'a Program<'a>, dir: PathBuf, file: Option<String>) -> Index<'a> {
  let mut f = Index {
    dir,
    file: file.unwrap_or_default(),
    locals: FxHashMap::default(),
    exports: FxHashMap::default(),
    reexports: FxHashMap::default(),
    stars: vec![],
  };
  let add_decl = |f: &mut Index<'a>, d: &'a Declaration<'a>, exported: bool| {
    let mut names: Vec<String> = vec![];
    match d {
      Declaration::VariableDeclaration(v) => {
        for decl in &v.declarations {
          if let BindingPattern::BindingIdentifier(id) = &decl.id {
            let n = id.name.as_str().to_string();
            f.locals.insert(
              n.clone(),
              Local::Value(decl.type_annotation.as_ref().map(|a| &a.type_annotation)),
            );
            names.push(n);
          }
        }
      }
      Declaration::FunctionDeclaration(func) => {
        if let Some(id) = &func.id {
          let n = id.name.as_str().to_string();
          if let Local::Funcs(v) = f.locals.entry(n.clone()).or_insert_with(|| Local::Funcs(vec![]))
          {
            v.push(func);
          }
          names.push(n);
        }
      }
      Declaration::ClassDeclaration(c) => {
        if let Some(id) = &c.id {
          let n = id.name.as_str().to_string();
          f.locals.insert(n.clone(), Local::Class(c));
          names.push(n);
        }
      }
      Declaration::TSTypeAliasDeclaration(a) => {
        let n = a.id.name.as_str().to_string();
        f.locals.insert(n.clone(), Local::Alias(a));
        names.push(n);
      }
      Declaration::TSInterfaceDeclaration(i) => {
        let n = i.id.name.as_str().to_string();
        if let Local::Iface(v) = f.locals.entry(n.clone()).or_insert_with(|| Local::Iface(vec![])) {
          v.push(i);
        }
        names.push(n);
      }
      Declaration::TSEnumDeclaration(e) => {
        let n = e.id.name.as_str().to_string();
        f.locals.insert(n.clone(), Local::Other);
        names.push(n);
      }
      _ => {}
    }
    if exported {
      for n in names {
        f.exports.insert(n.clone(), n);
      }
    }
  };
  for stmt in &program.body {
    match stmt {
      Statement::ImportDeclaration(i) => {
        let spec = i.source.value.as_str().unwrap_or_default().to_string();
        for s in i.specifiers.iter().flatten() {
          let (local, entry) = match s {
            ImportDeclarationSpecifier::ImportSpecifier(s) => {
              (s.local.name.as_str(), Local::Import(spec.clone(), export_name(&s.imported)))
            }
            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
              (s.local.name.as_str(), Local::ImportNs(spec.clone()))
            }
            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
              (s.local.name.as_str(), Local::Import(spec.clone(), "default".into()))
            }
          };
          f.locals.insert(local.to_string(), entry);
        }
      }
      Statement::ExportDeclaration(e) => add_decl(&mut f, &e.declaration, true),
      Statement::ExportNamedDeclaration(e) => {
        for sp in &e.specifiers {
          f.exports.insert(export_name(&sp.exported), export_name(&sp.local));
        }
      }
      Statement::ExportFromDeclaration(e) => {
        let src = e.source.value.as_str().unwrap_or_default().to_string();
        for sp in &e.specifiers {
          f.reexports.insert(export_name(&sp.exported), (src.clone(), export_name(&sp.local)));
        }
      }
      Statement::ExportAllDeclaration(e) => {
        let src = e.source.value.as_str().unwrap_or_default().to_string();
        match &e.exported {
          None => f.stars.push(src),
          Some(n) => {
            f.locals.insert(export_name(n), Local::ImportNs(src));
            f.exports.insert(export_name(n), export_name(n));
          }
        }
      }
      _ => {
        if let Some(d) = stmt.as_declaration() {
          add_decl(&mut f, d, false);
        }
      }
    }
  }
  f
}
