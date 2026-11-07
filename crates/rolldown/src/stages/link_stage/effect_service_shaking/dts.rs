//! Type bounds from the `.d.ts` files the installed `effect` ships (Tim's decision on the
//! issue's second assumption: Effect's public types bound value flow).
//!
//! A minimal resolver, with no type checker. For each parameter and the return value of an
//! exported function it answers one question: which kinds of value can the declared type hold?
//! Primitives, functions, arrays, data-only records, objects that have certain members, or
//! anything (open). It follows imports and re-exports between `.d.ts` files, aliases, unions,
//! intersections (any one member is a superset), conditional types (both branches), interfaces
//! with their bases and classes. Type parameters, `unknown`, `any`, `object`, mapped and indexed
//! types, `typeof`, and anything it cannot follow are open, so a gap in the resolver never drops a
//! flow. See design section 8.

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
/// every member named there (own fields or prototype chain).
#[derive(Clone, Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per kind of value a type admits")]
pub struct Ty {
  pub open: bool,
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

  fn join(&mut self, o: Ty) {
    self.open |= o.open;
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

/// The bounds of one exported function: a type per parameter, and the return type (`None` is
/// open).
#[derive(Clone, Debug, Default)]
pub struct Bounds {
  pub params: Vec<Option<Ty>>,
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

fn export_name(n: &ModuleExportName<'_>) -> String {
  n.name().to_string()
}

impl Dts {
  /// The bounds of the function exported as `name` by the `.d.ts` at `path`, called with
  /// `nparams` parameters. Overloads with that many parameters are joined (all of them if none
  /// has), so a bound holds for every signature a call could match.
  pub fn bounds(&self, path: &Path, name: &str, nparams: usize) -> Bounds {
    let start = std::time::Instant::now();
    let out = self.bounds_inner(path, name, nparams);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  fn bounds_inner(&self, path: &Path, name: &str, nparams: usize) -> Bounds {
    let Some(sigs) = self.signatures(path, name) else {
      return Bounds { params: vec![None; nparams], ret: None };
    };
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
    let mut ret: Option<Ty> = None;
    for sig in &matching {
      let t = sig.ret.map_or_else(Ty::open, |t| self.classify(sig.file, t, &sig.tparams, 0));
      match &mut ret {
        Some(x) => x.join(t),
        None => ret = Some(t),
      }
    }
    Bounds { params, ret: ret.and_then(closed) }
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
        index(program, dir)
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
        // Any member is a superset of the intersection: the first that is not open.
        i.types
          .iter()
          .map(|m| self.classify(f, m, tp, depth + 1))
          .find(|c| !c.open)
          .unwrap_or_else(Ty::open)
      }
      TSType::TSConditionalType(c) => {
        // The result is one of the branches; `infer` names are open.
        let mut out = self.classify(f, &c.true_type, tp, depth + 1);
        out.join(self.classify(f, &c.false_type, tp, depth + 1));
        out
      }
      TSType::TSTypeLiteral(lit) => self.members_of(f, &lit.members, tp, depth),
      TSType::TSTypeReference(r) => self.reference(f, r, tp, depth),
      // `any`, `unknown`, `object`, `this`, mapped and indexed types, `typeof`, `import()`,
      // `infer` and anything else: open.
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
          "Record" => match arg(1) {
            Some(v) if self.classify(f, v, tp, depth + 1).prim_only() => {
              Ty { data_record: true, ..Ty::default() }
            }
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

fn index<'a>(program: &'a Program<'a>, dir: PathBuf) -> Index<'a> {
  let mut f = Index {
    dir,
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
