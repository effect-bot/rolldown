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
use rolldown_utils::rayon::{IntoParallelRefIterator, ParallelIterator};
use rustc_hash::FxHashMap;
use self_cell::self_cell;
use sugar_path::SugarPath;

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
  /// An `Effect` whose success is an object type that mentions no type parameter.
  pub effect_concrete: bool,
  pub prim: bool,
  /// A type parameter whose values enter the call only inside a service-free container
  /// (`Dts::container_only`): an unknown value that holds no service. `effect_free` is an
  /// `Effect` succeeding with one.
  pub free: bool,
  pub effect_free: bool,
  pub other: bool,
  /// An object type that mentions no type parameter: what the call returns holds nothing the
  /// caller handed it (direction 2: results at concrete types are service-free).
  pub concrete: bool,
  pub unknown: bool,
}

/// What `Dts::returned_call` reads: bounds, return shape, generic positions, callback masks,
/// which parameters take callbacks whose returns hold no service, and which are opaque values of
/// a type parameter (`Dts::opaque_positions`).
pub type ReturnedCall = (Bounds, RetShape, (Vec<bool>, bool), Vec<u8>, Vec<bool>, Vec<bool>);

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
  /// The file's source text, for `Dts::mentions_tparam`.
  source: &'a str,
  dir: PathBuf,
  /// The file's name (`Tracer.d.ts`).
  file: String,
  locals: FxHashMap<String, Local<'a>>,
  /// Function declarations by local name. A function may share its name with an interface (a
  /// declaration merge, `interface decodeTo` and `function decodeTo`), which `locals` keeps
  /// for type references.
  funcs: FxHashMap<String, Vec<&'a oxc::ast::ast::Function<'a>>>,
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

// SAFETY: as for `rolldown_ecmascript::EcmaAst`: the `Allocator` is `Send`, and the arena and
// the source move with the cell. Every reference in the index points into them, and nothing
// outside the cell refers into them. `Dts::preload` parses a file on one thread and hands it
// over whole; it is only read after that.
unsafe impl Send for DtsFile {}

/// A `.d.ts` file parsed into its index (`Dts::file`, `Dts::preload`).
fn parse(path: &Path, source: String) -> DtsFile {
  let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
  DtsFile::new(Owner { source, allocator: Allocator::default() }, |owner| {
    let ret = Parser::new(&owner.allocator, &owner.source, SourceType::d_ts()).parse();
    let program: &Program<'_> = owner.allocator.alloc(ret.program);
    index(program, dir, path.file_name().map(|n| n.to_string_lossy().to_string()))
  })
}

/// What `Dts::preload` found at a path.
pub enum Read {
  Missing,
  Unreadable(std::io::Error),
  Parsed,
}

struct Sig<'a> {
  params: Vec<(Option<&'a TSType<'a>>, bool)>,
  ret: Option<&'a TSType<'a>>,
  tparams: Vec<String>,
  /// The declared constraint of each of the signature's own type parameters, in `tparams`'s
  /// order. Type parameters added after them (a returned function's outer ones) have none
  /// recorded, and count as constrained.
  constraints: Vec<Option<&'a TSType<'a>>>,
  file: usize,
  /// For the function a call returns (`returned_sigs`): the parameter types of the call that
  /// returned it, with their file. Its outer type parameters may occur there.
  outer: Vec<(usize, &'a TSType<'a>)>,
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
          self.sig_ret_shape(sig, &mut out);
        }
      }
    }
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  /// The return shape of one signature. A type parameter a value of which enters the call only
  /// inside a service-free container (`container_only`) holds no service, so a return of one,
  /// or an `Effect` succeeding with one, is an unknown value that holds none (`RetShape::free`).
  fn sig_ret_shape(&self, sig: &Sig<'_>, out: &mut RetShape) {
    use oxc::span::GetSpan;
    match sig.ret {
      Some(t) => {
        let free = self.container_only(sig, t.span());
        self.ret_shape_of(sig.file, t, &sig.tparams, &free, out, 0);
      }
      None => out.unknown = true,
    }
  }

  fn ret_shape_of(
    &self,
    f: usize,
    t: &TSType<'_>,
    tp: &[String],
    free: &[String],
    out: &mut RetShape,
    depth: u32,
  ) {
    if depth > 8 {
      out.unknown = true;
      return;
    }
    match t {
      TSType::TSParenthesizedType(p) => {
        self.ret_shape_of(f, &p.type_annotation, tp, free, out, depth + 1);
      }
      TSType::TSUnionType(u) => {
        for m in &u.types {
          self.ret_shape_of(f, m, tp, free, out, depth + 1);
        }
      }
      TSType::TSTypeReference(r) => {
        if let TSTypeName::IdentifierReference(id) = &r.type_name {
          if tp.iter().any(|p| p == id.name.as_str()) {
            if r.type_arguments.is_none() && free.iter().any(|p| p == id.name.as_str()) {
              out.free = true;
            } else {
              out.param = true;
            }
            return;
          }
        }
        if let Some((rf, local)) = self.name_in(f, &r.type_name, 0) {
          let idx = self.index(rf);
          if idx.file == "Effect.d.ts" && local == "Effect" {
            out.effect = true;
            let success = r.type_arguments.as_ref().and_then(|a| a.params.first());
            let bare = |names: &[String]| {
              matches!(success, Some(TSType::TSTypeReference(sr))
                if sr.type_arguments.is_none()
                  && matches!(&sr.type_name, TSTypeName::IdentifierReference(id)
                    if names.iter().any(|p| p == id.name.as_str())))
            };
            match success {
              _ if bare(free) => out.effect_free = true,
              Some(TSType::TSTypeReference(sr)) if matches!(&sr.type_name, TSTypeName::IdentifierReference(id) if tp.iter().any(|p| p == id.name.as_str())) =>
              {
                out.effect_param = true;
              }
              Some(st) if self.classify(f, st, tp, depth + 1).prim_only() => {}
              Some(st)
                if !self.mentions_tparam(f, st, tp)
                  && !self.classify(f, st, tp, depth + 1).open =>
              {
                out.effect_concrete = true;
              }
              _ => out.effect_other = true,
            }
            return;
          }
        }
        let c = self.classify(f, t, tp, depth + 1);
        if c.prim_only() {
          out.prim = true;
        } else if !c.open && !c.func && !self.mentions_tparam(f, t, tp) {
          out.concrete = true;
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
        } else if !c.func && !self.mentions_tparam(f, t, tp) {
          out.concrete = true;
        } else {
          out.other = true;
        }
      }
    }
  }

  /// Whether the declared type `t` (in file `f`) mentions one of the type parameters `tp`, by
  /// name anywhere in its text: function types and members included. A false positive only
  /// makes a position generic.
  fn mentions_tparam(&self, f: usize, t: &TSType<'_>, tp: &[String]) -> bool {
    use oxc::span::GetSpan;
    if tp.is_empty() {
      return false;
    }
    let span = t.span();
    let src = self.index(f).source;
    let Some(text) = src.get(span.start as usize..span.end as usize) else { return true };
    let bytes = text.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    tp.iter().any(|p| {
      text.match_indices(p.as_str()).any(|(i, _)| {
        let before = i == 0 || !word(bytes[i - 1]);
        let end = i + p.len();
        let after = end >= bytes.len() || !word(bytes[end]);
        before && after
      })
    })
  }

  /// For a call to the function exported as `name` with `nparams` arguments: whether each
  /// parameter (and the rest parameter) is generic, its declared type mentioning a type
  /// parameter or being one the resolver can't follow. Values at the other, concrete, positions
  /// join no slot (direction 2). Joined over the overloads that fit; `None` without a signature.
  pub fn generic_positions(
    &self,
    path: &Path,
    name: &str,
    nparams: usize,
  ) -> Option<(Vec<bool>, bool)> {
    let start = std::time::Instant::now();
    let sigs = self.signatures(path, name)?;
    let out = self.generic_of_sigs(&sigs, nparams);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    Some(out)
  }

  /// For a call to the function exported as `name` with `nparams` arguments: per argument,
  /// whether its declared type is, in every signature the call may match, a type parameter
  /// whose constraint names no member (none, `unknown` or `any`; for a rest parameter, an
  /// array of those). Code generic in such a type reads no member of the value by name, as an
  /// `unknown`/`any` position reads none (assumption 2). `None` without a signature.
  pub fn opaque_positions(&self, path: &Path, name: &str, nparams: usize) -> Option<Vec<bool>> {
    let start = std::time::Instant::now();
    let sigs = self.signatures(path, name)?;
    let out = self.opaque_of_sigs(&sigs, nparams);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    Some(out)
  }

  fn opaque_of_sigs(&self, sigs: &[Sig<'_>], nparams: usize) -> Vec<bool> {
    let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
    let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
    let mut out = vec![!matching.is_empty(); nparams];
    for sig in matching {
      for (i, opaque) in out.iter_mut().enumerate() {
        *opaque &= match sig.params.get(i).or_else(|| sig.params.last().filter(|(_, r)| *r)) {
          Some((Some(t), rest)) => opaque_tparam(sig, t, *rest),
          _ => false,
        };
      }
    }
    out
  }

  /// For a call with `nparams` arguments: per parameter, which parameters of the callbacks its
  /// declared type holds (directly, in a union, or as members of an options type) take a value
  /// of a type parameter, as a bit mask over the first 8. A callback parameter of a concrete
  /// type (`(response: HttpClientResponse) => …`) receives no value the call was handed.
  /// All bits are set where the type is open or a type parameter.
  pub fn callback_masks(&self, path: &Path, name: &str, nparams: usize) -> Option<Vec<u8>> {
    let start = std::time::Instant::now();
    let sigs = self.signatures(path, name)?;
    let out = self.callback_masks_of(&sigs, nparams);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    Some(out)
  }

  /// For a call with `nparams` arguments: per parameter, whether every callback its declared
  /// type admits is declared to return a value that holds no service, in every signature the
  /// call may match. That is a primitive (`Predicate<A>`'s `(a: A) => boolean`), a type
  /// parameter a value of which leaves the call only inside a Schedule (`Schedule.map`'s
  /// `Output2`, assumption 4; `container_only`), or an `Effect` that succeeds with one of
  /// those. Anything else is not: a type parameter that may come back out, `unknown`, an
  /// object, a callback type given as a type parameter, an options object.
  pub fn callback_free_returns(
    &self,
    path: &Path,
    name: &str,
    nparams: usize,
  ) -> Option<Vec<bool>> {
    let start = std::time::Instant::now();
    let sigs = self.signatures(path, name)?;
    let out = self.free_returns_of(&sigs, nparams);
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    Some(out)
  }

  fn free_returns_of(&self, sigs: &[Sig<'_>], nparams: usize) -> Vec<bool> {
    let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
    let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
    let mut out = vec![!matching.is_empty(); nparams];
    for sig in matching {
      for (i, free) in out.iter_mut().enumerate() {
        let t = sig.params.get(i).or_else(|| sig.params.last().filter(|(_, r)| *r));
        *free &= match t {
          Some((Some(t), _)) => self.callback_returns_free(sig, t, 0),
          _ => false,
        };
      }
    }
    out
  }

  /// Whether `t`, a parameter type of `sig`, is a callback type, or a union of them and
  /// primitives, whose every call signature returns a value that holds no service.
  fn callback_returns_free(&self, sig: &Sig<'_>, t: &TSType<'_>, depth: u32) -> bool {
    use oxc::span::GetSpan;
    if depth > 6 {
      return false;
    }
    let f = sig.file;
    let tp = &sig.tparams;
    match t {
      TSType::TSParenthesizedType(p) => {
        self.callback_returns_free(sig, &p.type_annotation, depth + 1)
      }
      TSType::TSUnionType(u) => {
        let callable = |x: &TSType<'_>| !self.classify(f, x, tp, depth + 1).prim_only();
        u.types.iter().any(callable)
          && u.types.iter().all(|x| !callable(x) || self.callback_returns_free(sig, x, depth + 1))
      }
      TSType::TSTypeReference(r)
        if matches!(&r.type_name, TSTypeName::IdentifierReference(id)
          if tp.iter().any(|p| p == id.name.as_str())) =>
      {
        false
      }
      // A callback written in the signature: its return may be a type parameter of the
      // signature that leaves the call only inside a Schedule.
      TSType::TSFunctionType(ft) => {
        let ret = &ft.return_type.type_annotation;
        let own: Vec<String> = ft
          .type_parameters
          .as_ref()
          .map(|d| d.params.iter().map(|p| p.name.name.as_str().to_string()).collect())
          .unwrap_or_default();
        let free: Vec<String> =
          self.container_only(sig, ret.span()).into_iter().filter(|p| !own.contains(p)).collect();
        let mut tps = own;
        tps.extend(tp.iter().cloned());
        self.return_free(f, ret, &tps, &free, 0)
      }
      _ => {
        let mut sigs = vec![];
        self.sigs_of_type(f, t, &mut sigs, depth + 1);
        !sigs.is_empty()
          && sigs.iter().all(|s| {
            s.ret.is_some_and(|r| {
              let mut tps = s.tparams.clone();
              tps.extend(tp.iter().cloned());
              self.return_free(s.file, r, &tps, &[], 0)
            })
          })
      }
    }
  }

  /// Whether a callback's declared return `t` holds no service: primitives, a type parameter
  /// in `free`, or an `Effect` that succeeds with one of those (its failure and requirements
  /// carry no service: assumption 3, and requirements are types).
  fn return_free(
    &self,
    f: usize,
    t: &TSType<'_>,
    tp: &[String],
    free: &[String],
    depth: u32,
  ) -> bool {
    if depth > 6 {
      return false;
    }
    match t {
      TSType::TSParenthesizedType(p) => {
        self.return_free(f, &p.type_annotation, tp, free, depth + 1)
      }
      TSType::TSUnionType(u) => u.types.iter().all(|m| self.return_free(f, m, tp, free, depth + 1)),
      TSType::TSTypeReference(r) => {
        if let TSTypeName::IdentifierReference(id) = &r.type_name {
          if tp.iter().any(|p| p == id.name.as_str()) {
            return free.iter().any(|p| p == id.name.as_str()) && r.type_arguments.is_none();
          }
        }
        if self.is_public(f, &r.type_name, "Effect.d.ts", "Effect") {
          return r
            .type_arguments
            .as_ref()
            .and_then(|a| a.params.first())
            .is_some_and(|success| self.return_free(f, success, tp, free, depth + 1));
        }
        let c = self.classify(f, t, tp, 0);
        c.prim_only() && !c.ignored
      }
      _ => {
        let c = self.classify(f, t, tp, 0);
        c.prim_only() && !c.ignored
      }
    }
  }

  /// Whether the type name `n` in `f` is `local` of effect's public module `file`.
  fn is_public(&self, f: usize, n: &TSTypeName<'_>, file: &str, local: &str) -> bool {
    self.name_in(f, n, 0).is_some_and(|(rf, l)| {
      let idx = self.index(rf);
      l == local && idx.file == file && idx.dir.file_name().is_some_and(|d| d == "dist")
    })
  }

  /// The type parameters of `sig` a value of which is in a call only inside a service-free
  /// container: each occurs in the signature's parameter and return types (outside `own`, the
  /// type being read), or in the parameters of the call that returned the function (`outer`),
  /// at least once, and only in the type arguments `effect::SERVICE_FREE_CONTAINERS` lists
  /// (any of a Schedule's, assumption 4; a Sink's result and leftovers, assumption 6). No
  /// service passes through those, so such a value holds none. The scan is by name over the declaration's
  /// text: a name it can't place counts as an occurrence outside a container.
  fn container_only(&self, sig: &Sig<'_>, own: oxc::span::Span) -> Vec<String> {
    use oxc::ast_visit::Visit;
    use oxc::span::GetSpan;
    struct Containers<'d> {
      dts: &'d Dts,
      f: usize,
      spans: Vec<(usize, oxc::span::Span)>,
    }
    impl<'a> Visit<'a> for Containers<'_> {
      fn visit_ts_type_reference(&mut self, r: &oxc::ast::ast::TSTypeReference<'a>) {
        let container = super::effect::SERVICE_FREE_CONTAINERS
          .iter()
          .find(|(file, local, _)| self.dts.is_public(self.f, &r.type_name, file, local));
        let Some((_, _, args)) = container else {
          oxc::ast_visit::walk::walk_ts_type_reference(self, r);
          return;
        };
        let Some(a) = &r.type_arguments else { return };
        for (i, t) in a.params.iter().enumerate() {
          if args.is_empty() || args.contains(&i) {
            self.spans.push((self.f, t.span()));
          } else {
            self.visit_ts_type(t);
          }
        }
      }
    }
    let types: Vec<(usize, &TSType<'_>)> = sig
      .params
      .iter()
      .filter_map(|(t, _)| t.map(|t| (sig.file, t)))
      .chain(sig.ret.map(|t| (sig.file, t)))
      .chain(sig.outer.iter().copied())
      .collect();
    let mut spans = vec![];
    for (f, t) in &types {
      let mut c = Containers { dts: self, f: *f, spans: vec![] };
      c.visit_ts_type(t);
      spans.extend(c.spans);
    }
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    sig
      .tparams
      .iter()
      .filter(|p| {
        let mut inside = false;
        for (f, t) in &types {
          let src = self.index(*f).source;
          let span = t.span();
          let Some(text) = src.get(span.start as usize..span.end as usize) else { return false };
          let bytes = text.as_bytes();
          for (i, _) in text.match_indices(p.as_str()) {
            let end = i + p.len();
            if (i > 0 && word(bytes[i - 1])) || (end < bytes.len() && word(bytes[end])) {
              continue;
            }
            #[expect(
              clippy::cast_possible_truncation,
              reason = "declaration files are under 4 GiB"
            )]
            let at = span.start + i as u32;
            if *f == sig.file && own.start <= at && at < own.end {
              continue;
            }
            if !spans.iter().any(|(sf, s)| sf == f && s.start <= at && at < s.end) {
              return false;
            }
            inside = true;
          }
        }
        inside
      })
      .cloned()
      .collect()
  }

  fn callback_masks_of(&self, sigs: &[Sig<'_>], nparams: usize) -> Vec<u8> {
    let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
    let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
    let mut masks = vec![0u8; nparams];
    for sig in matching {
      for (i, mask) in masks.iter_mut().enumerate() {
        let t = match sig.params.get(i).or_else(|| sig.params.last().filter(|(_, r)| *r)) {
          Some((t, _)) => *t,
          None => {
            *mask = 0xFF;
            continue;
          }
        };
        *mask |= match t {
          None => 0xFF,
          Some(t) => self.mask_of(sig.file, t, &sig.tparams, 0),
        };
      }
    }
    masks
  }

  fn mask_of(&self, f: usize, t: &TSType<'_>, tp: &[String], depth: u32) -> u8 {
    if depth > 6 {
      return 0xFF;
    }
    match t {
      TSType::TSFunctionType(ft) => {
        let mut m = 0u8;
        for (j, p) in ft.params.items.iter().take(8).enumerate() {
          let generic = p.type_annotation.as_ref().is_none_or(|a| {
            self.mentions_tparam(f, &a.type_annotation, tp)
              || self.classify(f, &a.type_annotation, tp, 0).open
          });
          if generic {
            m |= 1 << j;
          }
        }
        if ft.params.rest.is_some() {
          m |= !((1u8 << ft.params.items.len().min(7)) - 1);
        }
        m
      }
      TSType::TSParenthesizedType(p) => self.mask_of(f, &p.type_annotation, tp, depth + 1),
      TSType::TSUnionType(u) => {
        u.types.iter().fold(0, |m, x| m | self.mask_of(f, x, tp, depth + 1))
      }
      TSType::TSIntersectionType(u) => {
        u.types.iter().fold(0, |m, x| m | self.mask_of(f, x, tp, depth + 1))
      }
      TSType::TSTypeLiteral(lit) => lit.members.iter().fold(0, |m, member| {
        m | match member {
          TSSignature::TSPropertySignature(p) => p
            .type_annotation
            .as_ref()
            .map_or(0, |a| self.mask_of(f, &a.type_annotation, tp, depth + 1)),
          TSSignature::TSIndexSignature(ix) => {
            self.mask_of(f, &ix.type_annotation.type_annotation, tp, depth + 1)
          }
          TSSignature::TSMethodSignature(_) => 0xFF,
          _ => 0,
        }
      }),
      TSType::TSTypeReference(r) => {
        // A type parameter, or a named type the callbacks of which aren't read here: open,
        // unless the type mentions no type parameter and no function at all.
        if let TSTypeName::IdentifierReference(id) = &r.type_name {
          if tp.iter().any(|p| p == id.name.as_str()) {
            // A value of a type parameter may be any callback: every parameter is open. Its
            // constraint is not read, since reading it could only open fewer parameters, and
            // the pass may use a type only to keep more (Tim's condition on deviation 4).
            return 0xFF;
          }
        }
        let c = self.classify(f, t, tp, depth + 1);
        if c.prim_only() || (!c.func && !c.open && !self.mentions_tparam(f, t, tp)) {
          0
        } else if self.mentions_tparam(f, t, tp) || c.open {
          0xFF
        } else {
          // A concrete named type that holds functions (an interface of handlers): their
          // parameters are concrete too.
          0
        }
      }
      _ => {
        let c = self.classify(f, t, tp, depth + 1);
        if c.open || self.mentions_tparam(f, t, tp) { 0xFF } else { 0 }
      }
    }
  }

  fn generic_of_sigs(&self, sigs: &[Sig<'_>], nparams: usize) -> (Vec<bool>, bool) {
    let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
    let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
    let mut params = vec![false; nparams];
    let mut rest = false;
    for sig in matching {
      let generic = |t: Option<&TSType<'_>>| match t {
        None => true,
        Some(t) => {
          self.mentions_tparam(sig.file, t, &sig.tparams)
            || self.classify(sig.file, t, &sig.tparams, 0).open
        }
      };
      for (i, (t, is_rest)) in sig.params.iter().enumerate() {
        let g = generic(*t);
        if *is_rest {
          rest |= g;
          for p in params.iter_mut().skip(i) {
            *p |= g;
          }
        } else if let Some(p) = params.get_mut(i) {
          *p |= g;
        }
      }
      // Arguments beyond the declared parameters: open.
      for p in params.iter_mut().skip(sig.params.len()) {
        if !sig.params.last().is_some_and(|(_, r)| *r) {
          *p = true;
        }
      }
    }
    (params, rest)
  }

  /// The call signatures of the function a call to the export `name` with `nparams` arguments
  /// returns (a data-last form's `(self: X) => Y`), with the outer type parameters in scope.
  /// `None` when a matching signature's return has no call signature to read.
  fn returned_sigs(&self, path: &Path, name: &str, nparams: usize) -> Option<Vec<Sig<'_>>> {
    let sigs = self.signatures(path, name)?;
    let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
    let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
    let mut inner = vec![];
    for sig in matching {
      let ret = sig.ret?;
      let before = inner.len();
      self.sigs_of_type(sig.file, ret, &mut inner, 0);
      if inner.len() == before {
        return None;
      }
      for s in &mut inner[before..] {
        s.tparams.extend(sig.tparams.iter().cloned());
        s.outer.extend(sig.params.iter().filter_map(|(t, _)| t.map(|t| (sig.file, t))));
        s.outer.extend(sig.outer.iter().copied());
      }
    }
    Some(inner)
  }

  /// For a call with `nargs` arguments of the function the export `name` returns when called
  /// with `nparams`: its parameter bounds, return shape and generic positions. `None` when the
  /// returned function's signature can't be read.
  pub fn returned_call(
    &self,
    path: &Path,
    name: &str,
    nparams: usize,
    nargs: usize,
  ) -> Option<ReturnedCall> {
    let start = std::time::Instant::now();
    let inner = self.returned_sigs(path, name, nparams);
    let out = inner.map(|inner| {
      let mut bounds = self.bounds_of_sigs(&inner, nargs);
      internal_open(path, &mut bounds);
      let mut shape = RetShape::default();
      let same: Vec<&Sig<'_>> = inner.iter().filter(|s| s.params.len() == nargs).collect();
      let matching: Vec<&Sig<'_>> = if same.is_empty() { inner.iter().collect() } else { same };
      for sig in matching {
        self.sig_ret_shape(sig, &mut shape);
      }
      (
        bounds,
        shape,
        self.generic_of_sigs(&inner, nargs),
        self.callback_masks_of(&inner, nargs),
        self.free_returns_of(&inner, nargs),
        self.opaque_of_sigs(&inner, nargs),
      )
    });
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  /// The member names the interfaces and classes of the `.d.ts` at `path` declare only as
  /// methods (or function-typed properties), never as data properties. A read of one off a
  /// value of the module is a method, not a value of a type parameter (assumption 2).
  pub fn method_only_names(&self, path: &Path) -> rustc_hash::FxHashSet<String> {
    let start = std::time::Instant::now();
    let mut methods = rustc_hash::FxHashSet::default();
    let mut props = rustc_hash::FxHashSet::default();
    if let Some(f) = self.file(path) {
      let idx = self.index(f);
      let key =
        |k: &oxc::ast::ast::PropertyKey<'_>| k.static_name().map(|n| n.display().to_string());
      for l in idx.locals.values() {
        match l {
          Local::Iface(is) => {
            for i in is {
              for m in &i.body.body {
                match m {
                  TSSignature::TSMethodSignature(ms) => {
                    if let Some(n) = key(&ms.key) {
                      methods.insert(n);
                    }
                  }
                  TSSignature::TSPropertySignature(ps) => {
                    if let Some(n) = key(&ps.key) {
                      let func = matches!(
                        ps.type_annotation.as_ref().map(|a| strip_parens(&a.type_annotation)),
                        Some(TSType::TSFunctionType(_))
                      );
                      if func {
                        methods.insert(n)
                      } else {
                        props.insert(n)
                      };
                    }
                  }
                  _ => {}
                }
              }
            }
          }
          Local::Class(c) => {
            for e in &c.body.body {
              match e {
                ClassElement::MethodDefinition(m) => {
                  if let Some(n) = key(&m.key) {
                    methods.insert(n);
                  }
                }
                ClassElement::PropertyDefinition(p) => {
                  if let Some(n) = key(&p.key) {
                    props.insert(n);
                  }
                }
                ClassElement::AccessorProperty(p) => {
                  if let Some(n) = key(&p.key) {
                    props.insert(n);
                  }
                }
                _ => {}
              }
            }
          }
          _ => {}
        }
      }
    }
    for p in &props {
      methods.remove(p);
    }
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    methods
  }

  /// Whether a method of a value the `.d.ts` at `path` declares may store its argument in that
  /// value: some interface or class there has a method (or function-typed property) with a
  /// parameter whose type is one of the interface's own type parameters, or an array or
  /// iterable of one (`publish(value: A)` on `PubSub.Atomic<A>`). Effect's data types are
  /// otherwise immutable, their containers written through functions (`Ref.set`). True when
  /// the file can't be read.
  pub fn methods_store(&self, path: &Path) -> bool {
    let start = std::time::Instant::now();
    let out = self.file(path).is_none_or(|f| {
      let idx = self.index(f);
      idx.locals.values().any(|l| match l {
        Local::Iface(is) => is.iter().any(|i| {
          let tp: Vec<String> = i
            .type_parameters
            .as_deref()
            .map(|d| d.params.iter().map(|p| p.name.name.as_str().to_string()).collect())
            .unwrap_or_default();
          !tp.is_empty() && i.body.body.iter().any(|m| member_stores(m, &tp))
        }),
        Local::Class(c) => {
          let tp: Vec<String> = c
            .type_parameters
            .as_deref()
            .map(|d| d.params.iter().map(|p| p.name.name.as_str().to_string()).collect())
            .unwrap_or_default();
          !tp.is_empty()
            && c.body.body.iter().any(|e| match e {
              ClassElement::MethodDefinition(m) => params_store(&m.value.params, &tp),
              _ => false,
            })
        }
        _ => false,
      })
    });
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  /// `(stores, reads)` for a call to the function exported as `name` with `nparams` arguments,
  /// or to the function it returns. `stores`: the call may store one argument in a value
  /// another argument holds (a container's setter): some type parameter is the type of one
  /// position (`value: A`, `ReadonlyArray<A>`, `Iterable<A>`) and an argument of a type
  /// reference at another (`self: Ref<A>`). A data-last form counts the parameters of the
  /// function it returns as positions too (`set(value: A): (self: Ref<A>) => …`). Callback
  /// parameters (`f: (a: A) => B`) are read positions, not stores. `reads`: some type parameter is
  /// an argument of a type reference at a position (`self: Ref<A>`) and appears in what the call
  /// finally returns (`Effect<A>`), so the call may hand back what a container holds. Both are
  /// true without a signature.
  pub fn container_shape(&self, path: &Path, name: &str, nparams: usize) -> (bool, bool) {
    let start = std::time::Instant::now();
    let class = self
      .export(path, name, 0)
      .is_some_and(|(f, local)| matches!(self.index(f).locals.get(&local), Some(Local::Class(_))));
    let out = match self.signatures(path, name) {
      // A class's instances hold what they are constructed with themselves.
      None if class => (false, false),
      None => (true, true),
      Some(sigs) => {
        let same: Vec<&Sig<'_>> = sigs.iter().filter(|s| s.params.len() == nparams).collect();
        let matching: Vec<&Sig<'_>> = if same.is_empty() { sigs.iter().collect() } else { same };
        let mut out = (false, false);
        for sig in matching {
          let mut positions: Vec<&TSType<'_>> = sig.params.iter().filter_map(|(t, _)| *t).collect();
          let mut tparams = sig.tparams.clone();
          let mut ret = sig.ret;
          if let Some(TSType::TSFunctionType(ft)) = sig.ret.map(strip_parens) {
            positions.extend(
              ft.params
                .items
                .iter()
                .filter_map(|p| p.type_annotation.as_ref())
                .map(|a| &a.type_annotation),
            );
            if let Some(tps) = ft.type_parameters.as_deref() {
              tparams.extend(tps.params.iter().map(|p| p.name.name.as_str().to_string()));
            }
            ret = Some(&ft.return_type.type_annotation);
          }
          let uses: Vec<(Vec<String>, Vec<String>)> = positions
            .iter()
            .map(|t| {
              let (mut bare, mut nested) = (vec![], vec![]);
              tparams_in(t, &tparams, true, &mut bare, &mut nested, 0);
              (bare, nested)
            })
            .collect();
          out.0 |= uses.iter().enumerate().any(|(i, (bare, _))| {
            bare.iter().any(|p| uses.iter().enumerate().any(|(j, (_, n))| j != i && n.contains(p)))
          });
          let returned: Vec<String> = match ret {
            Some(r) => {
              let (mut bare, mut nested) = (vec![], vec![]);
              tparams_in(r, &tparams, true, &mut bare, &mut nested, 0);
              bare.into_iter().chain(nested).collect()
            }
            None => tparams.clone(),
          };
          out.1 |= uses.iter().any(|(_, n)| n.iter().any(|p| returned.contains(p)));
        }
        out
      }
    };
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out
  }

  /// A service constructor (Tim's decision on NodeFileSystem, option (b)): a function exported
  /// as `name` that takes an implementation and returns a named type, or an `Effect` or `Layer`
  /// of one. An implementation is a parameter typed as a function (`HttpClient.make(f)`) or by
  /// a type that names the returned one (`FileSystem.make(impl: Omit<FileSystem, …>)`,
  /// `makeNoop(fs: Partial<FileSystem>)`); a parameter of data (`fromWeb(request: Request)`,
  /// `layerCurrentZone(zone: TimeZone)`) is not one. Returns the returned type's declaring
  /// `.d.ts` and local name for each such signature; the caller checks that the name is a
  /// service key's.
  pub fn constructed_shapes(&self, path: &Path, name: &str) -> Vec<(PathBuf, String)> {
    let start = std::time::Instant::now();
    let mut out = vec![];
    for sig in self.signatures(path, name).unwrap_or_default() {
      let Some(mut ret) = sig.ret else { continue };
      let mut file = sig.file;
      for _ in 0..2 {
        let TSType::TSTypeReference(r) = ret else { break };
        let Some((rf, local)) = self.name_in(file, &r.type_name, 0) else { break };
        let idx = self.index(rf);
        let wrapper = (idx.file == "Effect.d.ts" && local == "Effect")
          || (idx.file == "Layer.d.ts" && local == "Layer");
        if !wrapper {
          let takes_impl = sig.params.iter().any(|(t, _)| {
            t.is_some_and(|t| {
              matches!(strip_parens(t), TSType::TSFunctionType(_)) || mentions(t, &local, 0)
            })
          });
          if takes_impl && !sig.tparams.contains(&local) {
            out.push((idx.dir.join(&idx.file), local));
          }
          break;
        }
        match r.type_arguments.as_ref().and_then(|a| a.params.first()) {
          Some(t) => {
            ret = t;
            file = sig.file;
          }
          None => break,
        }
      }
    }
    self.resolve_ns.set(self.resolve_ns.get() + start.elapsed().as_nanos());
    out.sort();
    out.dedup();
    out
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

  /// The bounds of the function that the function exported as `name` returns, which takes
  /// `nparams` parameters: the call signatures of each declared return type, with the outer type
  /// parameters still open. Only a module analysed under the service-constructor exception needs
  /// this: `internal/glob.js`'s `make` returns the `glob` method `FileSystem.make` installs. A
  /// type-modelled export's returned function is bounded by `returned_call` instead.
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
    // Relative specifiers resolve lexically, so `dist/http/../Effect.d.ts` is `dist/Effect.d.ts`:
    // each file is parsed once, under its normalized path, whichever way it was reached.
    let normalized = path.normalize();
    if normalized != path {
      let f = self.file(&normalized);
      self.by_path.borrow_mut().insert(path.to_path_buf(), f);
      return f;
    }
    let parsed = std::fs::read_to_string(path).ok().map(|source| self.add(parse(path, source)));
    self.by_path.borrow_mut().insert(path.to_path_buf(), parsed);
    parsed
  }

  fn add(&self, file: DtsFile) -> usize {
    self.files_parsed.set(self.files_parsed.get() + 1);
    self.files.push(file)
  }

  /// Reads and parses the files at `paths` in parallel, ahead of the lookups that need them
  /// (`file`), and reports for each whether it is missing or can't be read. A file that is
  /// neither is parsed once, under its normalized path; one that is is left to `file`.
  pub fn preload(&self, paths: &[PathBuf]) -> Vec<Read> {
    let reads: Vec<(PathBuf, Result<DtsFile, Read>)> = paths
      .par_iter()
      .map(|path| {
        let path = path.normalize().into_owned();
        let read = if path.is_file() {
          std::fs::read_to_string(&path)
            .map(|source| parse(&path, source))
            .map_err(Read::Unreadable)
        } else {
          Err(Read::Missing)
        };
        (path, read)
      })
      .collect();
    reads
      .into_iter()
      .map(|(path, read)| match read {
        Ok(file) => {
          if !self.by_path.borrow().contains_key(&path) {
            let f = self.add(file);
            self.by_path.borrow_mut().insert(path, Some(f));
          }
          Read::Parsed
        }
        Err(read) => read,
      })
      .collect()
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
    let idx = self.index(f);
    let funcs = idx.funcs.get(&local).map(|fs| Local::Funcs(fs.clone()));
    match funcs.as_ref().or_else(|| idx.locals.get(&local))? {
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

/// Whether a parameter list takes a value of one of `tp` (bare, or as array elements).
fn params_store(params: &FormalParameters<'_>, tp: &[String]) -> bool {
  params.items.iter().filter_map(|p| p.type_annotation.as_ref()).any(|a| {
    let (mut bare, mut nested) = (vec![], vec![]);
    tparams_in(&a.type_annotation, tp, true, &mut bare, &mut nested, 0);
    !bare.is_empty()
  })
}

/// Whether an interface member is a method that takes a value of one of `tp`.
fn member_stores(m: &TSSignature<'_>, tp: &[String]) -> bool {
  match m {
    TSSignature::TSMethodSignature(ms) => params_store(&ms.params, tp),
    TSSignature::TSPropertySignature(ps) => {
      match ps.type_annotation.as_ref().map(|a| &a.type_annotation) {
        Some(TSType::TSFunctionType(ft)) => params_store(&ft.params, tp),
        _ => false,
      }
    }
    _ => false,
  }
}

fn strip_parens<'a>(t: &'a TSType<'a>) -> &'a TSType<'a> {
  match t {
    TSType::TSParenthesizedType(p) => strip_parens(&p.type_annotation),
    _ => t,
  }
}

/// Whether `t` refers to a type named `name` outside function types (`Omit<FileSystem, …>`).
fn mentions(t: &TSType<'_>, name: &str, depth: u32) -> bool {
  if depth > 8 {
    return false;
  }
  let d = depth + 1;
  match t {
    TSType::TSTypeReference(r) => {
      let named = match &r.type_name {
        TSTypeName::IdentifierReference(id) => id.name.as_str() == name,
        TSTypeName::QualifiedName(q) => q.right.name.as_str() == name,
        TSTypeName::ThisExpression(_) => false,
      };
      named
        || r.type_arguments.as_ref().is_some_and(|a| a.params.iter().any(|p| mentions(p, name, d)))
    }
    TSType::TSArrayType(a) => mentions(&a.element_type, name, d),
    TSType::TSTypeOperatorType(o) => mentions(&o.type_annotation, name, d),
    TSType::TSParenthesizedType(p) => mentions(&p.type_annotation, name, d),
    TSType::TSUnionType(u) => u.types.iter().any(|m| mentions(m, name, d)),
    TSType::TSIntersectionType(u) => u.types.iter().any(|m| mentions(m, name, d)),
    _ => false,
  }
}

/// The type parameters `t` mentions: `bare` where a value of the parameter itself (or an element
/// of an array or iterable of it) sits at the position, `nested` where it is an argument of
/// another type reference or a property type. Function types are skipped: their parameters are
/// read, not stored.
fn tparams_in(
  t: &TSType<'_>,
  tp: &[String],
  bare_ctx: bool,
  bare: &mut Vec<String>,
  nested: &mut Vec<String>,
  depth: u32,
) {
  if depth > 8 {
    return;
  }
  let d = depth + 1;
  match t {
    TSType::TSTypeReference(r) => {
      if let TSTypeName::IdentifierReference(id) = &r.type_name {
        let n = id.name.as_str();
        if tp.iter().any(|p| p == n) {
          let list = if bare_ctx { &mut *bare } else { &mut *nested };
          if !list.iter().any(|p| p == n) {
            list.push(n.to_string());
          }
          return;
        }
      }
      let elems = matches!(&r.type_name, TSTypeName::IdentifierReference(id) if matches!(
        id.name.as_str(),
        "Array" | "ReadonlyArray" | "Iterable" | "NonEmptyArray" | "NonEmptyReadonlyArray"
      ));
      if let Some(args) = &r.type_arguments {
        for a in &args.params {
          tparams_in(a, tp, bare_ctx && elems, bare, nested, d);
        }
      }
    }
    TSType::TSArrayType(a) => tparams_in(&a.element_type, tp, bare_ctx, bare, nested, d),
    TSType::TSTypeOperatorType(o) => tparams_in(&o.type_annotation, tp, bare_ctx, bare, nested, d),
    TSType::TSParenthesizedType(p) => tparams_in(&p.type_annotation, tp, bare_ctx, bare, nested, d),
    TSType::TSUnionType(u) => {
      for m in &u.types {
        tparams_in(m, tp, bare_ctx, bare, nested, d);
      }
    }
    TSType::TSIntersectionType(u) => {
      for m in &u.types {
        tparams_in(m, tp, bare_ctx, bare, nested, d);
      }
    }
    TSType::TSTupleType(tt) => {
      for e in &tt.element_types {
        if let Some(t) = e.as_ts_type() {
          tparams_in(t, tp, bare_ctx, bare, nested, d);
        }
      }
    }
    TSType::TSTypeLiteral(lit) => {
      for m in &lit.members {
        if let TSSignature::TSPropertySignature(p) = m {
          if let Some(a) = &p.type_annotation {
            tparams_in(&a.type_annotation, tp, false, bare, nested, d);
          }
        }
      }
    }
    _ => {}
  }
}

/// Whether a value at a parameter declared `t` (its elements, for a rest parameter) is a
/// value of a type parameter of `sig` whose constraint names no member (`opaque_positions`).
fn opaque_tparam(sig: &Sig<'_>, t: &TSType<'_>, rest: bool) -> bool {
  // The element type of an array type: `T[]`, `readonly T[]`, `Array<T>`, `ReadonlyArray<T>`.
  fn elem<'b>(t: &'b TSType<'b>) -> Option<&'b TSType<'b>> {
    match t {
      TSType::TSParenthesizedType(p) => elem(&p.type_annotation),
      TSType::TSArrayType(a) => Some(&a.element_type),
      TSType::TSTypeOperatorType(o) => elem(&o.type_annotation),
      TSType::TSTypeReference(r) => match &r.type_name {
        TSTypeName::IdentifierReference(id)
          if matches!(id.name.as_str(), "Array" | "ReadonlyArray") =>
        {
          r.type_arguments.as_ref().filter(|a| a.params.len() == 1).map(|a| &a.params[0])
        }
        _ => None,
      },
      _ => None,
    }
  }
  let open = |t: &TSType<'_>| matches!(t, TSType::TSAnyKeyword(_) | TSType::TSUnknownKeyword(_));
  // A bare type parameter of `sig`, with its constraint (`None` if it has none).
  let tparam = |t: &TSType<'_>| -> Option<Option<&TSType<'_>>> {
    let TSType::TSTypeReference(r) = strip_parens(t) else { return None };
    let TSTypeName::IdentifierReference(id) = &r.type_name else { return None };
    if r.type_arguments.is_some() {
      return None;
    }
    let i = sig.tparams.iter().position(|p| p == id.name.as_str())?;
    sig.constraints.get(i).copied()
  };
  let unconstrained = |c: Option<&TSType<'_>>| c.is_none_or(open);
  if !rest {
    return tparam(t).is_some_and(unconstrained);
  }
  // `...values: As` with `As extends ReadonlyArray<any>`, or `...values: Array<A>`.
  if let Some(c) = tparam(t) {
    return c.is_none_or(|c| open(c) || elem(c).is_some_and(open));
  }
  elem(t).is_some_and(|e| tparam(e).is_some_and(unconstrained))
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
  let constraints =
    tps.map(|d| d.params.iter().map(|p| p.constraint.as_ref()).collect()).unwrap_or_default();
  Sig { params: ps, ret, tparams, constraints, file, outer: vec![] }
}

fn index<'a>(program: &'a Program<'a>, dir: PathBuf, file: Option<String>) -> Index<'a> {
  let mut f = Index {
    source: program.source_text,
    dir,
    file: file.unwrap_or_default(),
    locals: FxHashMap::default(),
    funcs: FxHashMap::default(),
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
          f.funcs.entry(n.clone()).or_default().push(func);
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

#[cfg(test)]
mod tests {
  use super::Dts;

  /// Deviation (4) is gone: a callback declared as a type parameter has every parameter open,
  /// whatever its constraint says. Read through the constraint, `(value: string) => void` would
  /// close the parameter, and `(...args: Array<any>) => any` would ignore it.
  #[test]
  fn constrained_callback_type_parameter_is_open() {
    let dir =
      std::env::temp_dir().join(format!("rolldown-effect-dts-{}-constraint", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("index.d.ts");
    std::fs::write(
      &path,
      [
        "export declare const concrete: <F extends (value: string) => void>(f: F) => void;",
        "export declare const loose: <F extends (...args: Array<any>) => any>(f: F) => void;",
        "export declare const plain: (f: (value: string) => void) => void;",
        "",
      ]
      .join("\n"),
    )
    .unwrap();
    let dts = Dts::default();
    assert_eq!(dts.callback_masks(&path, "concrete", 1), Some(vec![0xFF]));
    assert_eq!(dts.callback_masks(&path, "loose", 1), Some(vec![0xFF]));
    // A callback declared with a concrete parameter type still receives nothing.
    assert_eq!(dts.callback_masks(&path, "plain", 1), Some(vec![0]));
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// A callback's return is filtered only where every signature the call may match declares
  /// it returns primitives: a predicate or refinement, not a type parameter, `unknown`, an
  /// object, or a callback type given as a type parameter.
  #[test]
  fn callback_prim_returns_need_a_primitive_return() {
    let dir =
      std::env::temp_dir().join(format!("rolldown-effect-dts-{}-prim-returns", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("index.d.ts");
    std::fs::write(
      &path,
      [
        "export interface Predicate<in A> { (a: A): boolean }",
        "export interface Refinement<in A, out B extends A> { (a: A): a is B }",
        "export declare const exists: {",
        "  <A, B extends A>(self: A, refinement: Refinement<A, B>): boolean;",
        "  <A>(self: A, predicate: Predicate<A>): boolean;",
        "};",
        "export declare const mixed: {",
        "  <A>(self: A, predicate: Predicate<A>): boolean;",
        "  <A, B>(self: A, f: (a: A) => B): B;",
        "};",
        "export declare const opaque: (f: (a: string) => unknown) => void;",
        "export declare const generic: <F extends () => boolean>(f: F) => void;",
        "export declare const object: (f: () => { a: string }) => void;",
        "",
      ]
      .join("\n"),
    )
    .unwrap();
    let dts = Dts::default();
    assert_eq!(dts.callback_free_returns(&path, "exists", 2), Some(vec![false, true]));
    assert_eq!(dts.callback_free_returns(&path, "mixed", 2), Some(vec![false, false]));
    assert_eq!(dts.callback_free_returns(&path, "opaque", 1), Some(vec![false]));
    assert_eq!(dts.callback_free_returns(&path, "generic", 1), Some(vec![false]));
    assert_eq!(dts.callback_free_returns(&path, "object", 1), Some(vec![false]));
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// A type parameter a callback returns holds no service only if it leaves the call inside a
  /// Schedule and nowhere else (assumption 4): not beside one, not into another callback, and
  /// not when it leaves the call not at all. An `Effect` succeeding with one is the same.
  #[test]
  fn callback_free_returns_through_a_schedule_only() {
    let dir = std::env::temp_dir()
      .join(format!("rolldown-effect-dts-{}-schedule-returns", std::process::id()))
      .join("dist");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
      dir.join("Effect.d.ts"),
      "export interface Effect<A, E = never> { a: A; e: E }\n",
    )
    .unwrap();
    let path = dir.join("Schedule.d.ts");
    std::fs::write(
      &path,
      [
        "import type { Effect } from \"./Effect.js\";",
        "export interface Schedule<Output, Input = unknown> { o: Output; i: Input }",
        "export interface Duration { millis: number }",
        "export declare const map: {",
        "  <O2>(f: (o: number) => O2 | Effect<O2>): <I>(self: Schedule<number, I>) => Schedule<O2, I>;",
        "  <I, O2>(self: Schedule<number, I>, f: (o: number) => O2 | Effect<O2>): Schedule<O2, I>;",
        "};",
        "export declare const beside: <O2>(f: () => O2) => [Schedule<O2>, O2];",
        "export declare const handed: <O2>(f: () => O2, g: (o: O2) => void) => Schedule<O2>;",
        "export declare const dropped: <X>(f: () => Effect<X>) => Schedule<number>;",
        "export declare const delay: (f: () => Effect<Duration>) => Schedule<number>;",
        "export declare const cond: (f: () => boolean | Effect<boolean>) => Schedule<number>;",
        "",
      ]
      .join("\n"),
    )
    .unwrap();
    let dts = Dts::default();
    assert_eq!(dts.callback_free_returns(&path, "map", 1), Some(vec![true]));
    assert_eq!(dts.callback_free_returns(&path, "map", 2), Some(vec![false, true]));
    assert_eq!(dts.callback_free_returns(&path, "beside", 1), Some(vec![false]));
    // `g` returns `void`; `f`'s value goes on into `g`.
    assert_eq!(dts.callback_free_returns(&path, "handed", 2), Some(vec![false, true]));
    assert_eq!(dts.callback_free_returns(&path, "dropped", 1), Some(vec![false]));
    assert_eq!(dts.callback_free_returns(&path, "delay", 1), Some(vec![false]));
    assert_eq!(dts.callback_free_returns(&path, "cond", 1), Some(vec![true]));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
  }

  /// An Effect succeeding with a type parameter that enters the call only inside a Schedule
  /// (assumption 4) or only as a Sink's result or leftovers (assumption 6) holds no service,
  /// also when the container is an argument of the call that returned the function
  /// (`Sig::outer`). One that enters as a Sink's input, or also outside a container, flows.
  #[test]
  fn container_only_results() {
    let dir = std::env::temp_dir()
      .join(format!("rolldown-effect-dts-{}-container-results", std::process::id()))
      .join("dist");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
      dir.join("Effect.d.ts"),
      "export interface Effect<A, E = never, R = never> { a: A; e: E; r: R }\n",
    )
    .unwrap();
    std::fs::write(
      dir.join("Schedule.d.ts"),
      "export interface Schedule<Output, Input = unknown> { o: Output; i: Input }\n",
    )
    .unwrap();
    std::fs::write(
      dir.join("Sink.d.ts"),
      "export interface Sink<A, In = unknown, L = never> { a: A; i: In; l: L }\n",
    )
    .unwrap();
    let path = dir.join("Stream.d.ts");
    std::fs::write(
      &path,
      [
        "import type { Effect } from \"./Effect.js\";",
        "import type { Schedule } from \"./Schedule.js\";",
        "import type { Sink } from \"./Sink.js\";",
        "export interface Stream<A, E = never> { a: A; e: E }",
        "export declare const repeat: {",
        "  <O>(schedule: Schedule<O>): <A>(self: Effect<A>) => Effect<O>;",
        "  <A, O>(self: Effect<A>, schedule: Schedule<O>): Effect<O>;",
        "};",
        "export declare const beside: <O>(schedule: Schedule<O>, o: O) => Effect<O>;",
        "export declare const run: {",
        "  <A2, A, L>(sink: Sink<A2, A, L>): <E>(self: Stream<A, E>) => Effect<A2, E>;",
        "  <A2, A, L, E>(self: Stream<A, E>, sink: Sink<A2, A, L>): Effect<A2, E>;",
        "};",
        "export declare const leftovers: <L>(sink: Sink<number, unknown, L>) => Effect<L>;",
        "export declare const input: <A>(sink: Sink<number, A>) => Effect<A>;",
        "export declare const seeded: <A2>(sink: Sink<A2>, a: A2) => Effect<A2>;",
        "",
      ]
      .join("\n"),
    )
    .unwrap();
    let dts = Dts::default();
    let free =
      |s: &super::RetShape| s.effect && s.effect_free && !s.effect_param && !s.effect_other;
    let flows = |s: &super::RetShape| s.effect && s.effect_param && !s.effect_free;
    assert!(free(&dts.ret_shape(&path, "repeat", 2)));
    assert!(free(&dts.returned_call(&path, "repeat", 1, 1).unwrap().1));
    assert!(flows(&dts.ret_shape(&path, "beside", 2)));
    assert!(free(&dts.ret_shape(&path, "run", 2)));
    assert!(free(&dts.returned_call(&path, "run", 1, 1).unwrap().1));
    assert!(free(&dts.ret_shape(&path, "leftovers", 1)));
    assert!(flows(&dts.ret_shape(&path, "input", 1)));
    assert!(flows(&dts.ret_shape(&path, "seeded", 2)));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
  }

  /// A value of a type parameter whose constraint names no member is opaque to generic code; one
  /// whose constraint names members, or a type that only mentions a type parameter, is not.
  #[test]
  fn opaque_type_parameter_positions() {
    let dir = std::env::temp_dir()
      .join(format!("rolldown-effect-dts-{}-opaque", std::process::id()))
      .join("dist");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Stream.d.ts");
    std::fs::write(
      &path,
      [
        "export interface Stream<A> { a: A }",
        "export declare const make: <const As extends ReadonlyArray<any>>(...values: As) => Stream<As[number]>;",
        "export declare const of: <A>(...values: Array<A>) => Stream<A>;",
        "export declare const succeed: <A>(value: A) => Stream<A>;",
        "export declare const loose: <A extends unknown>(value: A, n: number) => Stream<A>;",
        "export declare const handlers: <H extends { run: () => void }>(h: H) => Stream<H>;",
        "export declare const record: <A>(h: { run: () => A }) => Stream<A>;",
        "export declare const both: { <A>(value: A): Stream<A>; <A>(value: { run: () => A }): Stream<A> };",
        "",
      ]
      .join("\n"),
    )
    .unwrap();
    let dts = Dts::default();
    assert_eq!(dts.opaque_positions(&path, "make", 2), Some(vec![true, true]));
    assert_eq!(dts.opaque_positions(&path, "of", 1), Some(vec![true]));
    assert_eq!(dts.opaque_positions(&path, "succeed", 1), Some(vec![true]));
    assert_eq!(dts.opaque_positions(&path, "loose", 2), Some(vec![true, false]));
    assert_eq!(dts.opaque_positions(&path, "handlers", 1), Some(vec![false]));
    assert_eq!(dts.opaque_positions(&path, "record", 1), Some(vec![false]));
    assert_eq!(dts.opaque_positions(&path, "both", 1), Some(vec![false]));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
  }
}
