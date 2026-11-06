//! From solved demand to edits: which properties of which object literals go,
//! the AST mutation, and the `StmtInfo` patch that lets Rolldown's tree shaking
//! drop what the removed methods were the only users of.

#![expect(clippy::print_stderr, reason = "stderr is the opt-in debug report")]

use arcstr::ArcStr;
use oxc::ast::ast::{IdentifierReference, ObjectExpression, Program};
use oxc::ast_visit::{Visit, VisitMut, walk, walk_mut};
use oxc::span::{GetSpan, Span};
use rolldown_common::{ModuleIdx, StmtInfoIdx, SymbolOrMemberExprRef};
use rustc_hash::{FxHashMap, FxHashSet};

use super::effect::PURE_RULES;
use super::generate::{DeclKind, LiteralInfo, PropInfo, Shape, Shared};
use super::heap::{
  Analysis, Demand, ObjId, ObjKind, PROTO_INSPECT, PROTO_JSON, PROTO_PRIMITIVE, VarId,
};
use super::host::INSPECT_CUSTOM;

#[derive(Default, Debug)]
pub struct Report {
  pub keys: Vec<KeyReport>,
  pub removed: Vec<(String, u32, String)>,
  pub bail_reasons: Vec<String>,
  pub bail_all: bool,
}

#[derive(Debug)]
pub struct KeyReport {
  pub key: String,
  pub demand: String,
  pub values: String,
  pub providers: usize,
  pub removed: usize,
  pub kept: usize,
  pub bailed: bool,
}

pub struct Edits {
  /// Removed property span starts per module.
  pub properties: FxHashMap<ModuleIdx, FxHashSet<u32>>,
}

/// Formatters read protocol methods on every object reachable from what they format.
fn apply_protocols(a: &mut Analysis) {
  for bits in [PROTO_JSON, PROTO_PRIMITIVE, PROTO_INSPECT] {
    let mut stack: Vec<ObjId> =
      a.objs.iter_enumerated().filter(|(_, o)| o.protocols & bits != 0).map(|(id, _)| id).collect();
    let mut seen: FxHashSet<ObjId> = FxHashSet::default();
    while let Some(obj) = stack.pop() {
      if !seen.insert(obj) {
        continue;
      }
      let kind = a.objs[obj].kind.clone();
      match kind {
        ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } | ObjKind::Closure(_) => {
          if bits & PROTO_JSON != 0 {
            a.objs[obj].demand.add(&ArcStr::from("toJSON"));
          }
          if bits & PROTO_PRIMITIVE != 0 {
            for n in ["toString", "valueOf", "@@toPrimitive"] {
              a.objs[obj].demand.add(&ArcStr::from(n));
            }
          }
          if bits & PROTO_INSPECT != 0 {
            a.objs[obj].demand.add(&ArcStr::from(INSPECT_CUSTOM));
          }
          // Primitive conversion is not recursive; serialization and inspection are.
          if bits & (PROTO_JSON | PROTO_INSPECT) != 0 {
            let mut next: Vec<VarId> = a.objs[obj].fields.values().copied().collect();
            next.extend(a.objs[obj].elem);
            next.extend(a.objs[obj].spread_from.iter().copied());
            for v in next {
              stack.extend(a.pts(v).iter().copied());
            }
          }
        }
        ObjKind::Context { .. } | ObjKind::Runtime { .. } | ObjKind::Fiber => {
          // Effect's own `toJSON`/inspect on these enumerate the entries' values.
          if bits & (PROTO_JSON | PROTO_INSPECT) != 0 {
            let all = a.all_values;
            stack.extend(a.pts(all).iter().copied());
          }
        }
        ObjKind::Promise { value } | ObjKind::Wrap(value) => {
          stack.extend(a.pts(value).iter().copied());
        }
        ObjKind::Effect { success, .. } if bits & (PROTO_JSON | PROTO_INSPECT) != 0 => {
          stack.extend(a.pts(success).iter().copied());
        }
        _ => {}
      }
    }
  }
}

fn demand_string(d: &Demand) -> String {
  match d {
    Demand::None => String::new(),
    Demand::All => "all".to_string(),
    Demand::Names(names) => {
      let mut v: Vec<&str> = names.iter().map(ArcStr::as_str).collect();
      v.sort_unstable();
      v.join(",")
    }
  }
}

/// Decides the edits. `module_names` is used for the report.
pub fn plan(
  a: &mut Analysis,
  sh: &Shared<'_>,
  module_names: &dyn Fn(ModuleIdx) -> String,
) -> (Edits, Report) {
  apply_protocols(a);
  let mut report =
    Report { bail_all: a.bail_all, bail_reasons: a.bail_reasons.clone(), ..Report::default() };

  // Reference keys are never pruned.
  for obj in &a.objs {
    if let ObjKind::Key { strings, reference: true } = obj.kind {
      for s in a.pts(strings).clone() {
        if let ObjKind::Str(name) = &a.objs[s].kind {
          if a.never_prune.insert(name.clone()) {
            a.bail_reasons
              .push(format!("key {name} is a Reference (key object from {:?})", obj.site));
          }
        }
      }
    }
  }

  // Per-key demand: the union over every provider, plus bail state.
  let keys: Vec<(ArcStr, VarId)> = a.key_values.iter().map(|(k, v)| (k.clone(), *v)).collect();
  let mut key_demand: FxHashMap<ArcStr, Demand> = FxHashMap::default();
  let mut candidate_keys: FxHashMap<ObjId, Vec<ArcStr>> = FxHashMap::default();
  for (key, var) in &keys {
    let mut d = Demand::None;
    if a.key_is_bailed(key) {
      d.all();
    }
    for obj in a.pts(*var).iter().copied() {
      match a.objs[obj].kind {
        ObjKind::Object | ObjKind::Array | ObjKind::Class { .. } | ObjKind::Closure(_) => {
          d.merge(&a.objs[obj].demand.clone());
          if a.objs[obj].escaped == Some(super::heap::Flavour::Ext) {
            d.all();
          }
          candidate_keys.entry(obj).or_default().push(key.clone());
        }
        ObjKind::Top(_) | ObjKind::Wrap(_) | ObjKind::External { .. } => d.all(),
        _ => {}
      }
    }
    key_demand.insert(key.clone(), d);
  }

  // Candidates: literals that are a key's value, then literals spread into candidates.
  let mut total: FxHashMap<ObjId, Demand> = FxHashMap::default();
  for (obj, ks) in &candidate_keys {
    if !sh.literals.contains_key(obj) {
      continue;
    }
    let mut d = a.objs[*obj].demand.clone();
    for k in ks {
      d.merge(&key_demand[k]);
    }
    total.insert(*obj, d);
  }
  loop {
    let mut changed = false;
    let targets: Vec<ObjId> = total.keys().copied().collect();
    for target in targets {
      // Sources spread into the target inherit its demand.
      let d = total[&target].clone();
      let sources: Vec<ObjId> =
        a.objs[target].spread_from.iter().flat_map(|v| a.pts(*v).iter().copied()).collect();
      for src in sources {
        if !sh.literals.contains_key(&src) {
          continue;
        }
        let fresh = !total.contains_key(&src);
        let own = a.objs[src].demand.clone();
        let entry = total.entry(src).or_insert(own.clone());
        let c1 = entry.merge(&d);
        let c2 = entry.merge(&own);
        if fresh || c1 || c2 {
          changed = true;
        }
      }
    }
    if !changed {
      break;
    }
  }

  if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_DEBUG").is_some() {
    for (obj, demand) in &total {
      let info: &LiteralInfo = &sh.literals[obj];
      let sources: Vec<String> = a.objs[*obj]
        .spread_from
        .iter()
        .flat_map(|v| a.pts(*v).iter().copied())
        .map(|s| format!("{s:?}:{}", sh.literals.contains_key(&s)))
        .collect();
      eprintln!(
        "[effect-service-shaking] candidate {:?} at {}:{} demand={} keys={:?} spread sources {:?}",
        obj,
        module_names(info.module),
        info.span_start,
        demand_string(demand),
        candidate_keys.get(obj),
        sources
      );
      for prop in &info.props {
        let Some(name) = &prop.name else { continue };
        let kinds: Vec<String> = a
          .pts(prop.value)
          .iter()
          .take(4)
          .map(|o| {
            format!("{:?}", a.objs[*o].kind).split([' ', '(', '{']).next().unwrap_or("").to_string()
          })
          .collect();
        eprintln!(
          "[effect-service-shaking]   {name}: demanded={} method={} safe={} shape={:?} value={:?}",
          demand.has(name),
          is_method(a, prop),
          initializer_is_safe(a, sh, info, prop),
          prop.shape,
          kinds
        );
      }
    }
  }
  let mut edits = Edits { properties: FxHashMap::default() };
  let mut per_key_removed: FxHashMap<ArcStr, usize> = FxHashMap::default();
  let mut per_key_kept: FxHashMap<ArcStr, usize> = FxHashMap::default();
  for (obj, demand) in &total {
    if demand.is_all() {
      continue;
    }
    let info: &LiteralInfo = &sh.literals[obj];
    for prop in &info.props {
      let Some(name) = &prop.name else { continue };
      if prop.accessor || prop.spread || name.as_str() == "__proto__" {
        continue;
      }
      let removable =
        !demand.has(name) && is_method(a, prop) && initializer_is_safe(a, sh, info, prop);
      let count = if removable { &mut per_key_removed } else { &mut per_key_kept };
      for k in candidate_keys.get(obj).into_iter().flatten() {
        *count.entry(k.clone()).or_default() += 1;
      }
      if removable {
        edits.properties.entry(info.module).or_default().insert(prop.span_start);
        report.removed.push((module_names(info.module), prop.span_start, name.to_string()));
      }
    }
  }
  for (key, _) in &keys {
    let demand = &key_demand[key];
    let providers = candidate_keys.iter().filter(|(_, ks)| ks.contains(key)).count();
    let var = a.key_values[key];
    let mut kinds: Vec<String> = a
      .pts(var)
      .iter()
      .take(8)
      .map(|o| {
        format!("{:?}", a.objs[*o].kind).split([' ', '(', '{']).next().unwrap_or("").to_string()
      })
      .collect();
    kinds.sort();
    kinds.dedup();
    report.keys.push(KeyReport {
      key: key.to_string(),
      demand: demand_string(demand),
      values: kinds.join("|"),
      providers,
      removed: per_key_removed.get(key).copied().unwrap_or(0),
      kept: per_key_kept.get(key).copied().unwrap_or(0),
      bailed: a.key_is_bailed(key),
    });
  }
  report.keys.sort_by(|x, y| x.key.cmp(&y.key));
  (edits, report)
}

/// The property's value is provably a function, or a primitive such as a `var` read before its
/// initialisation: removing it cannot drop an object some other path still uses.
fn is_method(a: &Analysis, prop: &PropInfo) -> bool {
  let pts = a.pts(prop.value);
  !pts.is_empty()
    && pts.iter().all(|o| match &a.objs[*o].kind {
      ObjKind::Closure(_) | ObjKind::Prim | ObjKind::Str(_) | ObjKind::Num(_) => true,
      ObjKind::Builtin { rule, .. } => {
        PURE_RULES.contains(&a.rules[*rule as usize].0)
          || a.rules[*rule as usize].0.ends_with(".fn")
          || a.rules[*rule as usize].0.ends_with(".wrapper")
          || a.rules[*rule as usize].0.ends_with(".partial")
      }
      _ => false,
    })
}

/// Removing the initializer must not change behaviour: no call that may have effects, no
/// identifier read that could throw.
fn initializer_is_safe(a: &Analysis, sh: &Shared<'_>, info: &LiteralInfo, prop: &PropInfo) -> bool {
  match &prop.shape {
    Shape::FnLiteral => true,
    Shape::Ident(sym) => {
      // Reading the binding must not throw: it is initialised before the literal is built
      // (textually, and not skipped by a `switch` case), or hoisted.
      let Some(decl) = sh.decls.get(sym) else { return false };
      match decl.kind {
        DeclKind::Function | DeclKind::Var => true,
        DeclKind::Param | DeclKind::Const | DeclKind::Let | DeclKind::Class => {
          let module_fn = sh.module_fns[info.module];
          let same_scope = decl.func == info.func || Some(decl.func) == module_fn;
          let same_case = decl.case.is_none() || decl.case == info.case;
          let initialised = decl.initialized_after <= info.span_start;
          same_scope && same_case && initialised
        }
        DeclKind::Import => false,
      }
    }
    Shape::Call { callee, simple_args } => {
      // A call whose callee only allocates: a pure model rule, or a function whose body just
      // returns a function literal (`fs => (pattern, options) => ...`).
      *simple_args
        && !a.pts(*callee).is_empty()
        && a.pts(*callee).iter().all(|o| match &a.objs[*o].kind {
          ObjKind::Builtin { rule, .. } => PURE_RULES.contains(&a.rules[*rule as usize].0),
          ObjKind::Closure(f) => a.fns[*f].returns_closure_only,
          _ => false,
        })
    }
    Shape::Other => false,
  }
}

// == AST edits

struct Pruner<'s> {
  properties: &'s FxHashSet<u32>,
  removed: Vec<Span>,
}

impl<'a> VisitMut<'a> for Pruner<'_> {
  fn visit_object_expression(&mut self, it: &mut ObjectExpression<'a>) {
    let mut removed = vec![];
    it.properties.retain(|p| {
      let span = p.span();
      if self.properties.contains(&span.start) {
        removed.push(span);
        false
      } else {
        true
      }
    });
    self.removed.extend(removed);
    walk_mut::walk_object_expression(self, it);
  }
}

/// Collects the symbols referenced by identifiers under a node.
#[derive(Default)]
struct Refs {
  symbols: FxHashSet<oxc::semantic::SymbolId>,
}

struct RefCollector<'s> {
  scoping: &'s oxc::semantic::Scoping,
  refs: Refs,
}

impl<'a> Visit<'a> for RefCollector<'_> {
  fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
    if let Some(symbol) =
      it.reference_id.get().and_then(|r| self.scoping.get_reference(r).symbol_id())
    {
      self.refs.symbols.insert(symbol);
    }
    walk::walk_identifier_reference(self, it);
  }
}

fn stmt_refs(
  scoping: &oxc::semantic::Scoping,
  program: &Program<'_>,
) -> Vec<FxHashSet<oxc::semantic::SymbolId>> {
  program
    .body
    .iter()
    .map(|stmt| {
      let mut c = RefCollector { scoping, refs: Refs::default() };
      c.visit_statement(stmt);
      c.refs.symbols
    })
    .collect()
}

/// Applies the edits to one module and patches its statement infos.
pub fn apply(
  ast: &mut rolldown_ecmascript::EcmaAst,
  scoping: &oxc::semantic::Scoping,
  stmt_infos: &mut rolldown_common::StmtInfos,
  module: ModuleIdx,
  properties: &FxHashSet<u32>,
) {
  let before = stmt_refs(scoping, ast.program());
  let mut removed_spans = vec![];
  ast.program.with_mut(|fields| {
    let mut pruner = Pruner { properties, removed: vec![] };
    pruner.visit_program(fields.program);
    removed_spans = pruner.removed;
  });
  let after = stmt_refs(scoping, ast.program());
  for (i, (b, af)) in before.iter().zip(after.iter()).enumerate() {
    let gone: FxHashSet<_> = b.difference(af).copied().collect();
    if gone.is_empty() && removed_spans.is_empty() {
      continue;
    }
    let idx = StmtInfoIdx::new(i + 1);
    if idx.index() >= stmt_infos.infos.len() {
      continue;
    }
    let info = stmt_infos.get_mut(idx);
    info.referenced_symbols.retain(|r| match r {
      SymbolOrMemberExprRef::Symbol(s) => !(s.owner == module && gone.contains(&s.symbol)),
      SymbolOrMemberExprRef::MemberExpr(m) => !removed_spans
        .iter()
        .any(|removed| removed.start <= m.span.start && m.span.end <= removed.end),
    });
  }
}
