//! The decision record (implementation.md, deferred generation): what one build's analysis
//! decided, named by source identity (module path, source offset, key and method names) instead
//! of variable, object or function ids, so that builds of the same input with eager and with
//! deferred generation compare. Written only when asked for (`LinkStage::effect_service_shaking`).
//!
//! The record is lossy by design. Decisions (bails, keys, removals, widening, activation,
//! literals and whether each is a candidate) are complete. Flows and escapes are supporting
//! evidence: objects are named by kind and site, so two objects a rule makes at one call site
//! for different callees (`Site::sub`) share a name, and so do the instances of one class;
//! counts tell such objects apart but not which is which. Clones are named by the call site
//! they were made for, and a clone's call sites by the clone, so the flows of different
//! callers of one function stay apart.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use rolldown_common::ModuleIdx;
use rustc_hash::{FxHashMap, FxHashSet};

use super::generate::Shared;
use super::heap::{Analysis, CLONE_SUB, FnId, ObjId, ObjKind, RULE_SUB, Site, VarId};
use super::prune::Report;

/// The record of the last solve of a build that ran `solves` of them. Lines are sorted within
/// each section. A literal inside a lazy statement the solve never activated is a `dormant
/// literal`: only eager generation allocates it, so the comparison checks those apart.
pub fn record(
  a: &Analysis,
  sh: &Shared<'_>,
  report: &Report,
  solves: usize,
  names: &dyn Fn(ModuleIdx) -> String,
) -> String {
  let id_sited = a.id_sited();
  let (clones, clone_sites) = a.clone_origins();
  let names = Names { a, names, id_sited: &id_sited, clones: &clones, clone_sites: &clone_sites };
  let obj = |o: ObjId| names.obj(o, 0);
  let fn_names = &names;
  let names = fn_names.names;
  // How many variables hold each object, and how many of those are linked to anything: have
  // a copy or a constraint installed, or are spread into an object. The generator seeds each
  // allocation into a fresh variable at once (`Gen::alloc`); what that variable feeds waits in
  // its body. An object that only unlinked variables hold took part in nothing: constraints
  // act on a variable's objects, and a copy would have put it in another variable.
  let spread: FxHashSet<VarId> =
    a.objs.iter().flat_map(|o| o.spread_from.iter().copied()).collect();
  let mut held: FxHashMap<ObjId, (usize, usize)> = FxHashMap::default();
  for (id, v) in a.vars.iter_enumerated() {
    let linked = !v.copies.is_empty() || !v.constraints.is_empty() || spread.contains(&id);
    for o in &v.pts {
      let entry = held.entry(*o).or_default();
      entry.0 += 1;
      entry.1 += usize::from(linked);
    }
  }
  let candidates: FxHashSet<ObjId> =
    a.key_values.values().flat_map(|v| a.pts(*v).iter().copied()).collect();
  let mut out = String::new();
  let _ = writeln!(out, "solves {solves}");
  let _ = writeln!(out, "bail_all {}", report.bail_all);
  let mut reasons: Vec<String> = a.bail_reasons.iter().map(|r| normalize(r, names)).collect();
  reasons.sort();
  for r in reasons {
    let _ = writeln!(out, "bail {r}");
  }
  let mut never: Vec<&str> = a.never_prune.iter().map(arcstr::ArcStr::as_str).collect();
  never.sort_unstable();
  let _ = writeln!(out, "never_prune {never:?}");
  for k in &report.keys {
    // The report samples the first 8 values in id order; the record takes every value's kind.
    let mut values: BTreeSet<String> =
      a.key_values.get(k.key.as_str()).map_or_else(BTreeSet::new, |v| {
        a.pts(*v)
          .iter()
          .map(|o| {
            format!("{:?}", a.objs[*o].kind).split([' ', '(', '{']).next().unwrap_or("").to_string()
          })
          .collect()
      });
    values.retain(|v| !v.is_empty());
    let values: Vec<String> = values.into_iter().collect();
    let _ = writeln!(
      out,
      "key {} providers={} values={} removed={} kept={} bailed={} demand={}",
      k.key,
      k.providers,
      values.join("|"),
      k.removed,
      k.kept,
      k.bailed,
      k.demand
    );
  }
  // Each key's values, one by one (supporting evidence).
  let mut providers: BTreeMap<String, usize> = BTreeMap::new();
  for (k, v) in &a.key_values {
    for o in a.pts(*v) {
      *providers.entry(format!("provider {k} {}", obj(*o))).or_default() += 1;
    }
  }
  for (p, n) in &providers {
    let _ = writeln!(out, "{p} x{n}");
  }
  let mut removed: Vec<String> = report
    .removed
    .iter()
    .map(|(m, start, name)| format!("removed {name} at {m}:{start}"))
    .collect();
  removed.sort();
  for r in &removed {
    let _ = writeln!(out, "{r}");
  }
  let _ = writeln!(out, "widened {}", a.widened_vars);

  // Lazy statements, and whether the solve activated each.
  let mut lazy: Vec<String> = sh
    .lazy_spans
    .iter()
    .map(|(m, start, end, f)| {
      format!("lazy {}:{start}-{end} activated={}", names(*m), a.fns[*f].activated)
    })
    .collect();
  lazy.sort();
  for l in &lazy {
    let _ = writeln!(out, "{l}");
  }

  // Literals: dormant ones apart, with the properties the plan removed from each.
  let removed_at: FxHashSet<(String, u32)> =
    report.removed.iter().map(|(m, start, _)| (m.clone(), *start)).collect();
  // A dormant literal also says whether it took part in anything: how many variables hold it,
  // whether it is a key's value, the demand on it and whether it escaped.
  let mut literals: Vec<String> = sh
    .literals
    .iter()
    .map(|(o, info)| {
      let module = names(info.module);
      let dormant = dormant(a, sh, info.module, info.span_start);
      let cut =
        info.props.iter().filter(|p| removed_at.contains(&(module.clone(), p.span_start))).count();
      let candidate = candidates.contains(o);
      if dormant {
        let demand = match &a.objs[*o].demand {
          super::heap::Demand::None => "none",
          super::heap::Demand::All => "all",
          super::heap::Demand::Names(_) => "names",
        };
        let escaped = a.objs[*o].escaped.is_some() || a.class_escaped.contains_key(o);
        let (held, linked) = held.get(o).copied().unwrap_or_default();
        format!(
          "dormant literal {module}:{} removed={cut} held={held} linked={linked} candidate={candidate} demand={demand} escaped={escaped}",
          info.span_start,
        )
      } else {
        format!("literal {module}:{} candidate={candidate} removed={cut}", info.span_start)
      }
    })
    .collect();
  literals.sort();
  for l in &literals {
    let _ = writeln!(out, "{l}");
  }

  // Every activated body, each clone by the call site it was made for: what flows in through
  // its this and parameters and back through its return and yields.
  let mut bodies: BTreeMap<String, (usize, BTreeMap<String, Counts>)> = BTreeMap::new();
  for (id, f) in a.fns.iter_enumerated().filter(|(_, f)| f.activated && !f.deferred) {
    let entry = bodies.entry(format!("{} {:?}", fn_names.body(id, 0), f.kind)).or_default();
    entry.0 += 1;
    let mut slots: Vec<(String, VarId)> = vec![
      ("this".into(), f.this),
      ("ret".into(), f.ret),
      ("yielded".into(), f.yielded),
      ("fail".into(), f.yielded_fail),
    ];
    for (i, p) in f.params.iter().enumerate() {
      slots.push((format!("param{i}"), *p));
    }
    if let Some(r) = f.rest {
      slots.push(("rest".into(), r));
    }
    for (slot, v) in slots {
      let set = entry.1.entry(slot).or_default();
      for o in &a.vars[v].pts {
        *set.entry(obj(*o)).or_default() += 1;
      }
    }
  }
  for (body, (count, slots)) in &bodies {
    let _ = writeln!(out, "body {body} x{count}");
    for (slot, objs) in slots.iter().filter(|(_, o)| !o.is_empty()) {
      let _ = writeln!(out, "  {slot} {objs:?}");
    }
  }

  // Escapes, to code the pass can't see and into class code, counted.
  let mut escapes: BTreeMap<String, usize> = BTreeMap::new();
  // A host global's object has no site: it is named by the global.
  let mut globals: FxHashMap<ObjId, &str> = FxHashMap::default();
  for (name, v) in &sh.globals {
    for o in &a.vars[*v].pts {
      if a.objs[*o].site.is_none() {
        globals.entry(*o).and_modify(|n| *n = (*n).min(name.as_str())).or_insert(name.as_str());
      }
    }
  }
  for (id, o) in a.objs.iter_enumerated() {
    if let Some(flavour) = o.escaped {
      let global = globals.get(&id).map(|n| format!(" (global {n})")).unwrap_or_default();
      *escapes.entry(format!("escaped {}{global} {flavour:?}", obj(id))).or_default() += 1;
    }
  }
  // A class hole registers itself when generated (`Analysis::hole`): eager generation registers
  // those of statements the solve never activates too.
  for (o, (class, how)) in &a.class_escaped {
    let line = match a.objs[*class].site {
      // What took part in nothing: no variable holds the object or the class.
      Some(s) if dormant(a, sh, s.module, s.start) => {
        // The hole's own registration (a `ClassEscape` into this class) links nothing else.
        let (mut holders, mut linked) = (0, 0);
        for (id, v) in a.vars.iter_enumerated() {
          if !v.pts.contains(o) && !v.pts.contains(class) {
            continue;
          }
          holders += 1;
          let other = v
            .constraints
            .iter()
            .any(|c| !matches!(c, super::heap::Constraint::ClassEscape(c, _) if c == class));
          linked += usize::from(!v.copies.is_empty() || other || spread.contains(&id));
        }
        format!(
          "dormant class escaped {} into {} how={how} held={holders} linked={linked}",
          obj(*o),
          obj(*class),
        )
      }
      _ => format!("class escaped {} into {} how={how}", obj(*o), obj(*class)),
    };
    *escapes.entry(line).or_default() += 1;
  }
  for (e, n) in &escapes {
    let _ = writeln!(out, "{e} x{n}");
  }
  out
}

/// Whether the source offset lies in a lazy statement the solve never activated.
fn dormant(a: &Analysis, sh: &Shared<'_>, module: ModuleIdx, at: u32) -> bool {
  sh.lazy_spans
    .iter()
    .any(|(m, start, end, f)| *m == module && *start <= at && at < *end && !a.fns[*f].activated)
}

/// Lines or names, counted.
type Counts = BTreeMap<String, usize>;

/// Names objects, sites and bodies by source identity.
struct Names<'r> {
  a: &'r Analysis,
  names: &'r dyn Fn(ModuleIdx) -> String,
  id_sited: &'r FxHashMap<ObjId, ObjId>,
  clones: &'r FxHashMap<FnId, (FnId, Site)>,
  clone_sites: &'r FxHashMap<u32, (Site, FnId)>,
}

/// How deep names nest (an instance of a class made in a clone for a call in a clone ...).
const MAX_DEPTH: usize = 4;

impl Names<'_> {
  /// An object by what it is and where the program makes it. Sites that hold ids are named by
  /// what the ids stand for: an instance or default prototype by its class or function, a
  /// clone's call site by the clone.
  fn obj(&self, o: ObjId, depth: usize) -> String {
    let a = self.a;
    let ob = &a.objs[o];
    let kind = match &ob.kind {
      ObjKind::Str(s) => format!("str {s:?}"),
      ObjKind::Num(n) => format!("num {n}"),
      ObjKind::Builtin { rule, .. } => format!("builtin {}", a.rules[*rule as usize].0),
      ObjKind::External { name, .. } => format!("external {name}"),
      ObjKind::Closure(f) if depth < MAX_DEPTH => format!("Closure {}", self.body(*f, depth + 1)),
      other => format!("{other:?}").split([' ', '(', '{']).next().unwrap_or("").to_string(),
    };
    if let Some(of) = self.id_sited.get(&o).filter(|_| depth < MAX_DEPTH) {
      return format!("{kind} of ({})", self.obj(*of, depth + 1));
    }
    match ob.site {
      Some(s) => format!("{kind}@{}", self.site(s, depth)),
      None => kind,
    }
  }

  /// A site: its module and offset, and for a clone's call site the clone. A rule's sub-site
  /// is marked `rule` (its tag is a variable id).
  fn site(&self, s: Site, depth: usize) -> String {
    if Analysis::is_wrap_clone_site(s) {
      return "wraps".to_string();
    }
    match s.sub & CLONE_SUB {
      CLONE_SUB => match self.clone_sites.get(&(s.sub & !CLONE_SUB)) {
        Some((template, clone)) if depth < MAX_DEPTH => {
          format!("{} in {}", self.site(*template, depth + 1), self.body(*clone, depth + 1))
        }
        _ => format!("{}:{}:clone", (self.names)(s.module), s.start),
      },
      RULE_SUB => format!("{}:{}:rule", (self.names)(s.module), s.start),
      _ => format!("{}:{}:{}", (self.names)(s.module), s.start, s.sub),
    }
  }

  /// A body by its function's position, and a clone by the call site it was made for.
  fn body(&self, f: FnId, depth: usize) -> String {
    let info = &self.a.fns[f];
    let at = format!("{}:{}", (self.names)(info.module), info.span_start);
    match self.clones.get(&f) {
      Some((_, site)) if depth < MAX_DEPTH => format!("{at} for [{}]", self.site(*site, depth + 1)),
      Some(_) => format!("{at} for [...]"),
      None => at,
    }
  }
}

/// A bail reason with its module indices replaced by module paths: indices follow load order.
fn normalize(reason: &str, names: &dyn Fn(ModuleIdx) -> String) -> String {
  let mut out = String::new();
  let mut rest = reason;
  while let Some(at) = rest.find("module") {
    let (head, tail) = rest.split_at(at + "module".len());
    out.push_str(head);
    let sep = if tail.starts_with(": ") {
      ": "
    } else if tail.starts_with(' ') {
      " "
    } else {
      ""
    };
    let digits: String = tail[sep.len()..].chars().take_while(char::is_ascii_digit).collect();
    if sep.is_empty() || digits.is_empty() {
      rest = tail;
      continue;
    }
    let name = digits.parse::<u32>().ok().map(|i| names(ModuleIdx::from_usize(i as usize)));
    let _ = write!(out, "{sep}{}", name.unwrap_or(digits.clone()));
    rest = &tail[sep.len() + digits.len()..];
  }
  out.push_str(rest);
  out
}
