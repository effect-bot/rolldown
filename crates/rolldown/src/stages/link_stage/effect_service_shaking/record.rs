//! The decision record (implementation.md, deferred generation): what one build's analysis
//! decided, named by source identity (module path, source offset, key and method names) instead
//! of variable, object or function ids, so that builds of the same input with eager and with
//! deferred generation compare. Written only when asked for (`LinkStage::effect_service_shaking`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use rolldown_common::ModuleIdx;
use rustc_hash::{FxHashMap, FxHashSet};

use super::generate::Shared;
use super::heap::{Analysis, CLONE_SUB, ObjId, ObjKind, RULE_SUB, VarId};
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
  let obj = |o: ObjId| describe(a, o, names, &id_sited, 0);
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
  let mut literals: Vec<String> = sh
    .literals
    .values()
    .map(|info| {
      let module = names(info.module);
      let dormant = dormant(a, sh, info.module, info.span_start);
      let cut =
        info.props.iter().filter(|p| removed_at.contains(&(module.clone(), p.span_start))).count();
      let what = if dormant { "dormant literal" } else { "literal" };
      format!("{what} {module}:{} removed={cut}", info.span_start)
    })
    .collect();
  literals.sort();
  for l in &literals {
    let _ = writeln!(out, "{l}");
  }

  // Every activated body, its clones together: how many, and what flows through its this,
  // parameters, return and yields.
  let mut bodies: BTreeMap<String, (usize, BTreeMap<&'static str, BTreeSet<String>>)> =
    BTreeMap::new();
  for f in a.fns.iter().filter(|f| f.activated && !f.deferred) {
    let entry =
      bodies.entry(format!("{}:{} {:?}", names(f.module), f.span_start, f.kind)).or_default();
    entry.0 += 1;
    let mut slots: Vec<(&'static str, VarId)> =
      vec![("this", f.this), ("ret", f.ret), ("yielded", f.yielded), ("fail", f.yielded_fail)];
    for (i, p) in f.params.iter().enumerate().take(PARAMS.len()) {
      slots.push((PARAMS[i], *p));
    }
    if let Some(r) = f.rest {
      slots.push(("rest", r));
    }
    for (slot, v) in slots {
      let set = entry.1.entry(slot).or_default();
      set.extend(a.vars[v].pts.iter().map(|o| obj(*o)));
    }
  }
  for (body, (count, slots)) in &bodies {
    let _ = writeln!(out, "body {body} x{count}");
    for (slot, objs) in slots.iter().filter(|(_, o)| !o.is_empty()) {
      let _ = writeln!(out, "  {slot} {objs:?}");
    }
  }

  // Escapes, to code the pass can't see and into class code.
  let mut escapes: BTreeSet<String> = BTreeSet::new();
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
      escapes.insert(format!("escaped {}{global} {flavour:?}", obj(id)));
    }
  }
  // A class hole registers itself when generated (`Analysis::hole`): eager generation registers
  // those of statements the solve never activates too.
  for (o, (class, how)) in &a.class_escaped {
    let what = match a.objs[*class].site {
      Some(s) if dormant(a, sh, s.module, s.start) => "dormant class escaped",
      _ => "class escaped",
    };
    escapes.insert(format!("{what} {} into {} how={how}", obj(*o), obj(*class)));
  }
  for e in &escapes {
    let _ = writeln!(out, "{e}");
  }
  out
}

/// Whether the source offset lies in a lazy statement the solve never activated.
fn dormant(a: &Analysis, sh: &Shared<'_>, module: ModuleIdx, at: u32) -> bool {
  sh.lazy_spans
    .iter()
    .any(|(m, start, end, f)| *m == module && *start <= at && at < *end && !a.fns[*f].activated)
}

const PARAMS: [&str; 6] = ["param0", "param1", "param2", "param3", "param4", "param5"];

/// An object by what it is and where the program makes it. Sites that hold ids are named by
/// what the ids stand for: an instance or default prototype by its class or function, a rule's
/// sub-site and a clone's call site by their kind.
fn describe(
  a: &Analysis,
  o: ObjId,
  names: &dyn Fn(ModuleIdx) -> String,
  id_sited: &FxHashMap<ObjId, ObjId>,
  depth: usize,
) -> String {
  let ob = &a.objs[o];
  let kind = match &ob.kind {
    ObjKind::Str(s) => format!("str {s:?}"),
    ObjKind::Num(n) => format!("num {n}"),
    ObjKind::Builtin { rule, .. } => format!("builtin {}", a.rules[*rule as usize].0),
    ObjKind::External { name, .. } => format!("external {name}"),
    other => format!("{other:?}").split([' ', '(', '{']).next().unwrap_or("").to_string(),
  };
  if let Some(of) = id_sited.get(&o).filter(|_| depth < 3) {
    return format!("{kind} of ({})", describe(a, *of, names, id_sited, depth + 1));
  }
  match ob.site {
    Some(s) => {
      let sub = match s.sub & CLONE_SUB {
        CLONE_SUB => "clone".to_string(),
        RULE_SUB => "rule".to_string(),
        _ => s.sub.to_string(),
      };
      format!("{kind}@{}:{}:{sub}", names(s.module), s.start)
    }
    None => kind,
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
