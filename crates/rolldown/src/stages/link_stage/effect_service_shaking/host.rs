//! Models of JavaScript builtins and host APIs: what each does with its
//! arguments. A name missing here is an opaque callee (arguments escape as host
//! data). A wrong entry is a compiler bug.

#![expect(clippy::many_single_char_names, reason = "rules are small closures over `a` and `c`")]

use arcstr::ArcStr;
use rustc_hash::FxHashMap;

use super::heap::{
  Analysis, Arg, Call, Constraint, Flavour, Key, ObjId, ObjKind, Protocol, Site, VarId,
};

pub const INSPECT_CUSTOM: &str = "@@for:nodejs.util.inspect.custom";

/// Node builtin module names (without the `node:` prefix).
pub const NODE_BUILTINS: &[&str] = &[
  "assert",
  "assert/strict",
  "async_hooks",
  "buffer",
  "child_process",
  "cluster",
  "console",
  "constants",
  "crypto",
  "dgram",
  "diagnostics_channel",
  "dns",
  "dns/promises",
  "domain",
  "events",
  "fs",
  "fs/promises",
  "http",
  "http2",
  "https",
  "inspector",
  "module",
  "net",
  "os",
  "path",
  "path/posix",
  "path/win32",
  "perf_hooks",
  "process",
  "punycode",
  "querystring",
  "readline",
  "readline/promises",
  "repl",
  "stream",
  "stream/consumers",
  "stream/promises",
  "stream/web",
  "string_decoder",
  "sys",
  "timers",
  "timers/promises",
  "tls",
  "trace_events",
  "tty",
  "url",
  "util",
  "util/types",
  "v8",
  "vm",
  "wasi",
  "worker_threads",
  "zlib",
  "test",
  "sqlite",
  "sea",
];

pub fn is_node_builtin(specifier: &str) -> bool {
  let name = specifier.strip_prefix("node:").unwrap_or(specifier);
  specifier.starts_with("node:") || specifier.starts_with("bun:") || NODE_BUILTINS.contains(&name)
}

// == helpers shared with the Effect model

pub fn arg(a: &mut Analysis, call: &Call, i: usize) -> VarId {
  if call.has_spread() {
    if let Some(any) = a.spread_any.get(&(call.site, call.ret)) {
      return *any;
    }
    let any = a.var();
    a.spread_any.insert((call.site, call.ret), any);
    for x in &call.args {
      match x {
        Arg::Var(v) => a.copy(*v, any),
        Arg::Spread(v) => a.constrain(*v, Constraint::ElemOf(any)),
      }
    }
    return any;
  }
  match call.args.get(i) {
    Some(Arg::Var(v)) => *v,
    _ => {
      let v = a.var();
      a.seed(v, a.prim);
      v
    }
  }
}

pub fn args_from(_a: &mut Analysis, call: &Call, i: usize) -> Vec<Arg> {
  if call.has_spread() {
    return call.args.clone();
  }
  call.args.iter().skip(i).cloned().collect()
}

pub fn this_of(a: &mut Analysis, call: &Call) -> VarId {
  match call.this {
    Some(v) => v,
    None => {
      let v = a.var();
      a.seed(v, a.prim);
      v
    }
  }
}

/// Calls whatever `callee` holds with the arguments; returns the result variable.
pub fn invoke(
  a: &mut Analysis,
  site: Site,
  callee: VarId,
  this: Option<VarId>,
  args: Vec<VarId>,
) -> VarId {
  let ret = a.var();
  let call = Call {
    site: site.sub(callee),
    this,
    args: args.into_iter().map(Arg::Var).collect(),
    ret,
    is_new: false,
    builtin: None,
  };
  a.constrain(callee, Constraint::Call(Box::new(call)));
  ret
}

pub fn invoke_args(
  a: &mut Analysis,
  site: Site,
  callee: VarId,
  this: Option<VarId>,
  args: Vec<Arg>,
) -> VarId {
  let ret = a.var();
  let call = Call { site: site.sub(callee), this, args, ret, is_new: false, builtin: None };
  a.constrain(callee, Constraint::Call(Box::new(call)));
  ret
}

pub fn prim_var(a: &mut Analysis) -> VarId {
  let v = a.var();
  a.seed(v, a.prim);
  v
}

pub fn host_var(a: &mut Analysis) -> VarId {
  let v = a.var();
  a.seed(v, a.top_host);
  v
}

/// A descriptor's `get` and `set`. The generator keeps a getter only as the value it returns,
/// so `get` is a function whose call gives back the property's values (a wrap of them: reads
/// on its result demand on those values). Calling `set` runs a setter with what it is given,
/// which the pass doesn't follow: `set` is a value from code the pass can't see, so what it is
/// handed bails.
fn descriptor_accessors(a: &mut Analysis, desc: ObjId, values: VarId, site: Site) {
  let get = a.field(desc, &ArcStr::from("get"));
  let wrapped = a.wrap_of(values, site);
  a.copy(wrapped, get);
  let set = a.field(desc, &ArcStr::from("set"));
  a.seed(set, a.top_ext);
}

/// The values an enumeration of the objects in `o` hands out: every field, through the
/// admission filter with the type model on (`Analysis::admitted_values`).
fn enumerated(a: &mut Analysis, o: VarId) -> VarId {
  a.constrain(o, Constraint::Enumerate);
  let values = a.var();
  a.constrain(o, Constraint::Load { key: Key::Unknown, dst: values });
  a.admitted_values(values)
}

pub fn new_array(a: &mut Analysis, site: Site, tag: u8) -> (ObjId, VarId, VarId) {
  let arr = a.site_obj(site, tag, |_| ObjKind::Array);
  let elem = a.elem(arr);
  let v = a.var();
  a.seed(v, arr);
  (arr, v, elem)
}

pub fn new_object(a: &mut Analysis, site: Site, tag: u8) -> (ObjId, VarId) {
  let obj = a.site_obj(site, tag, |_| ObjKind::Object);
  let v = a.var();
  a.seed(v, obj);
  (obj, v)
}

pub fn new_promise(a: &mut Analysis, site: Site, tag: u8) -> (ObjId, VarId, VarId) {
  let p = a.site_obj(site, tag, |a| {
    let value = a.var();
    ObjKind::Promise { value }
  });
  let ObjKind::Promise { value } = a.objs[p].kind else { unreachable!() };
  let v = a.var();
  a.seed(v, p);
  (p, v, value)
}

pub fn elem_of(a: &mut Analysis, v: VarId) -> VarId {
  let dst = a.var();
  a.constrain(v, Constraint::ElemOf(dst));
  dst
}

pub fn await_of(a: &mut Analysis, v: VarId) -> VarId {
  let dst = a.var();
  a.constrain(v, Constraint::AwaitOf(dst));
  dst
}

pub fn load(a: &mut Analysis, base: VarId, name: &str) -> VarId {
  let dst = a.var();
  a.constrain(base, Constraint::Load { key: Key::Name(ArcStr::from(name)), dst });
  dst
}

pub fn escape(a: &mut Analysis, v: VarId, flavour: Flavour) {
  a.constrain(v, Constraint::Escape(flavour, None));
}

// == registration

/// Registers every host rule and returns the global-name resolver.
#[expect(clippy::too_many_lines, reason = "one flat table of builtin rules")]
pub fn register(a: &mut Analysis) {
  // Object
  a.rule("Object", |a, c| {
    let v = arg(a, c, 0);
    a.copy(v, c.ret);
  });
  for name in ["Object.keys", "Object.getOwnPropertyNames", "Object.getOwnPropertySymbols"] {
    a.rule(name, |a, c| {
      let o = arg(a, c, 0);
      a.constrain(o, Constraint::Enumerate);
      let (_, v, elem) = new_array(a, c.site, 1);
      a.seed(elem, a.top_host);
      a.copy(v, c.ret);
    });
  }
  a.rule("Object.values", |a, c| {
    let o = arg(a, c, 0);
    let values = enumerated(a, o);
    let (_, v, elem) = new_array(a, c.site, 1);
    a.copy(values, elem);
    a.copy(v, c.ret);
  });
  a.rule("Object.entries", |a, c| {
    // `[key, value]` pairs.
    let o = arg(a, c, 0);
    let values = enumerated(a, o);
    let (_, v, elem) = new_array(a, c.site, 1);
    let (_, pv, pair) = new_array(a, c.site, 9);
    a.copy(values, pair);
    a.seed(pair, a.top_host);
    a.copy(pv, elem);
    a.copy(v, c.ret);
  });
  a.rule("Object.getOwnPropertyDescriptors", |a, c| {
    // An object of descriptors, each holding a value under `value`.
    let o = arg(a, c, 0);
    let values = enumerated(a, o);
    let (_, v) = new_object(a, c.site, 1);
    let (desc, dv) = new_object(a, c.site, 9);
    let value = a.field(desc, &ArcStr::from("value"));
    a.copy(values, value);
    descriptor_accessors(a, desc, values, c.site);
    a.constrain(v, Constraint::Store { key: Key::Unknown, src: dv });
    a.copy(v, c.ret);
  });
  a.rule("Object.assign", |a, c| {
    let target = arg(a, c, 0);
    for src in args_from(a, c, 1) {
      let v = match src {
        Arg::Var(v) | Arg::Spread(v) => v,
      };
      a.constrain(target, Constraint::SpreadTarget(v));
    }
    a.copy(target, c.ret);
  });
  for name in ["Object.freeze", "Object.seal", "Object.preventExtensions"] {
    a.rule(name, |a, c| {
      let v = arg(a, c, 0);
      a.copy(v, c.ret);
    });
  }
  a.rule("Object.create", |a, c| {
    let proto = arg(a, c, 0);
    let (obj, v) = new_object(a, c.site, 2);
    let pv = a.proto_var(obj);
    a.copy(proto, pv);
    if c.args.len() > 1 {
      // Property descriptors: their values land on the object under unknown names.
      let descs = arg(a, c, 1);
      let values = a.var();
      a.constrain(descs, Constraint::Load { key: Key::Unknown, dst: values });
      let inner = load(a, values, "value");
      a.constrain(v, Constraint::Store { key: Key::Unknown, src: inner });
      let getter = load(a, values, "get");
      let got = invoke(a, c.site, getter, Some(v), vec![]);
      a.constrain(v, Constraint::Store { key: Key::Unknown, src: got });
    }
    a.copy(v, c.ret);
  });
  for name in ["Object.getPrototypeOf", "Reflect.getPrototypeOf"] {
    a.rule(name, |a, c| {
      let o = arg(a, c, 0);
      a.constrain(o, Constraint::ProtoOf(c.ret));
    });
  }
  for name in ["Object.setPrototypeOf", "Reflect.setPrototypeOf"] {
    a.rule(name, |a, c| {
      let o = arg(a, c, 0);
      let p = arg(a, c, 1);
      a.constrain(o, Constraint::SetProto(p));
      a.copy(o, c.ret);
    });
  }
  for name in ["Object.defineProperty", "Reflect.defineProperty"] {
    a.rule(name, |a, c| {
      let o = arg(a, c, 0);
      let key = arg(a, c, 1);
      let desc = arg(a, c, 2);
      let value = load(a, desc, "value");
      a.constrain(o, Constraint::Store { key: Key::Var(key), src: value });
      let getter = load(a, desc, "get");
      let got = invoke(a, c.site, getter, Some(o), vec![]);
      a.constrain(o, Constraint::Store { key: Key::Var(key), src: got });
      let setter = load(a, desc, "set");
      let current = a.var();
      a.constrain(o, Constraint::Load { key: Key::Var(key), dst: current });
      invoke(a, c.site, setter, Some(o), vec![current]);
      a.copy(o, c.ret);
    });
  }
  a.rule("Object.defineProperties", |a, c| {
    let o = arg(a, c, 0);
    let descs = arg(a, c, 1);
    let values = a.var();
    a.constrain(descs, Constraint::Load { key: Key::Unknown, dst: values });
    let inner = load(a, values, "value");
    a.constrain(o, Constraint::Store { key: Key::Unknown, src: inner });
    let getter = load(a, values, "get");
    let got = invoke(a, c.site, getter, Some(o), vec![]);
    a.constrain(o, Constraint::Store { key: Key::Unknown, src: got });
    a.copy(o, c.ret);
  });
  a.rule("Object.fromEntries", |a, c| {
    let iter = arg(a, c, 0);
    let (obj, v) = new_object(a, c.site, 3);
    let pairs = elem_of(a, iter);
    let items = elem_of(a, pairs);
    let items = a.admitted_values(items);
    let elem = a.elem(obj);
    a.copy(items, elem);
    a.copy(v, c.ret);
  });
  for name in ["Object.hasOwn", "Reflect.has", "Object.prototype.hasOwnProperty"] {
    a.rule(name, |a, c| {
      let (o, k) = if c.this.is_some() && c.args.len() == 1 {
        (this_of(a, c), arg(a, c, 0))
      } else {
        (arg(a, c, 0), arg(a, c, 1))
      };
      let dst = a.var();
      a.constrain(o, Constraint::Load { key: Key::Var(k), dst });
      a.seed(c.ret, a.prim);
    });
  }
  for name in ["Object.getOwnPropertyDescriptor", "Reflect.getOwnPropertyDescriptor"] {
    a.rule(name, |a, c| {
      let o = arg(a, c, 0);
      let k = arg(a, c, 1);
      let (obj, v) = new_object(a, c.site, 4);
      let value = a.field(obj, &ArcStr::from("value"));
      a.constrain(o, Constraint::Load { key: Key::Var(k), dst: value });
      descriptor_accessors(a, obj, value, c.site);
      a.copy(v, c.ret);
    });
  }
  a.rule("Object.is", |a, c| a.seed(c.ret, a.prim));
  // Reflect
  // A receiver is the `this` of the accessors the target's class declares: class code when
  // the target is a class, its prototype or an instance (`Analysis::class_bail`).
  a.rule("Reflect.get", |a, c| {
    let o = arg(a, c, 0);
    let k = arg(a, c, 1);
    a.constrain(o, Constraint::Load { key: Key::Var(k), dst: c.ret });
    if c.args.len() > 2 || c.has_spread() {
      let receiver = arg(a, c, 2);
      a.reflect_receivers.push((o, receiver));
    }
  });
  a.rule("Reflect.set", |a, c| {
    let o = arg(a, c, 0);
    let k = arg(a, c, 1);
    let v = arg(a, c, 2);
    a.constrain(o, Constraint::Store { key: Key::Var(k), src: v });
    a.seed(c.ret, a.prim);
    if c.args.len() > 3 || c.has_spread() {
      let receiver = arg(a, c, 3);
      a.reflect_receivers.push((o, receiver));
    }
  });
  a.rule("Reflect.ownKeys", |a, c| {
    let o = arg(a, c, 0);
    a.constrain(o, Constraint::Enumerate);
    let (_, v, elem) = new_array(a, c.site, 1);
    a.seed(elem, a.top_host);
    a.copy(v, c.ret);
  });
  a.rule("Reflect.apply", |a, c| {
    let f = arg(a, c, 0);
    let this = arg(a, c, 1);
    let args = arg(a, c, 2);
    let r = invoke_args(a, c.site, f, Some(this), vec![Arg::Spread(args)]);
    a.copy(r, c.ret);
  });
  a.rule("Reflect.construct", |a, c| {
    let f = arg(a, c, 0);
    let args = arg(a, c, 1);
    let ret = a.var();
    let call = Call {
      site: c.site,
      this: None,
      args: vec![Arg::Spread(args)],
      ret,
      is_new: true,
      builtin: None,
    };
    a.constrain(f, Constraint::Call(Box::new(call)));
    a.copy(ret, c.ret);
  });
  a.rule("Reflect.deleteProperty", |a, c| a.seed(c.ret, a.prim));
  // Function.prototype
  a.rule("Function.prototype.call", |a, c| {
    let f = c_this(a, c);
    let this = arg(a, c, 0);
    let args = args_from(a, c, 1);
    let r = invoke_args(a, c.site, f, Some(this), args);
    a.copy(r, c.ret);
  });
  a.rule("Function.prototype.apply", |a, c| {
    let f = c_this(a, c);
    let this = arg(a, c, 0);
    let args = arg(a, c, 1);
    let r = invoke_args(a, c.site, f, Some(this), vec![Arg::Spread(args)]);
    a.copy(r, c.ret);
  });
  a.rule("Function.prototype.bind", |a, c| {
    let f = c_this(a, c);
    let this = arg(a, c, 0);
    let (obj, data) = new_object(a, c.site, 5);
    let ff = a.field(obj, &ArcStr::from("fn"));
    a.copy(f, ff);
    let tf = a.field(obj, &ArcStr::from("this"));
    a.copy(this, tf);
    let (_, av, elem) = new_array(a, c.site, 6);
    for x in args_from(a, c, 1) {
      match x {
        Arg::Var(v) => a.copy(v, elem),
        Arg::Spread(v) => a.constrain(v, Constraint::ElemOf(elem)),
      }
    }
    let af = a.field(obj, &ArcStr::from("args"));
    a.copy(av, af);
    let id = a.rule_id("bound").unwrap();
    let b = a.builtin(id, Some(data), None);
    a.seed(c.ret, b);
  });
  a.rule("bound", |a, c| {
    let data = c_data(c);
    let f = load(a, data, "fn");
    let this = load(a, data, "this");
    let bound = load(a, data, "args");
    let mut args = vec![Arg::Spread(bound)];
    args.extend(args_from(a, c, 0));
    let r = invoke_args(a, c.site, f, Some(this), args);
    a.copy(r, c.ret);
  });
  // Array
  a.rule("Array", |a, c| {
    let (_, v, elem) = new_array(a, c.site, 7);
    if c.args.len() > 1 {
      for x in args_from(a, c, 0) {
        if let Arg::Var(x) = x {
          a.copy(x, elem);
        }
      }
    }
    a.copy(v, c.ret);
  });
  a.rule("Array.isArray", |a, c| a.seed(c.ret, a.prim));
  a.rule("Array.of", |a, c| {
    let (_, v, elem) = new_array(a, c.site, 7);
    for x in args_from(a, c, 0) {
      match x {
        Arg::Var(x) => a.copy(x, elem),
        Arg::Spread(x) => a.constrain(x, Constraint::ElemOf(elem)),
      }
    }
    a.copy(v, c.ret);
  });
  a.rule("Array.from", |a, c| {
    let src = arg(a, c, 0);
    let (_, v, elem) = new_array(a, c.site, 7);
    let items = elem_of(a, src);
    if c.args.len() > 1 {
      let f = arg(a, c, 1);
      let p = prim_var(a);
      let r = invoke(a, c.site, f, None, vec![items, p]);
      a.copy(r, elem);
    } else {
      a.copy(items, elem);
    }
    a.copy(v, c.ret);
  });
  for name in [
    "Array.prototype.push",
    "Array.prototype.unshift",
    "Array.prototype.add",
    "Array.prototype.fill",
  ] {
    a.rule(name, |a, c| {
      let this = c_this(a, c);
      let elem = a.var();
      for x in args_from(a, c, 0) {
        match x {
          Arg::Var(x) => a.copy(x, elem),
          Arg::Spread(x) => a.constrain(x, Constraint::ElemOf(elem)),
        }
      }
      a.constrain(this, Constraint::Store { key: Key::Unknown, src: elem });
      a.copy(this, c.ret);
      a.seed(c.ret, a.prim);
    });
  }
  a.rule("Array.prototype.set", |a, c| {
    let this = c_this(a, c);
    let k = arg(a, c, 0);
    let v = arg(a, c, 1);
    a.constrain(this, Constraint::Store { key: Key::Unknown, src: k });
    a.constrain(this, Constraint::Store { key: Key::Unknown, src: v });
    a.copy(this, c.ret);
  });
  for name in [
    "Array.prototype.pop",
    "Array.prototype.shift",
    "Array.prototype.at",
    "Array.prototype.get",
    "Array.prototype.find",
    "Array.prototype.findLast",
    "Array.prototype.deref",
  ] {
    a.rule(name, |a, c| {
      let this = c_this(a, c);
      let elem = elem_of(a, this);
      a.copy(elem, c.ret);
      if !c.args.is_empty() {
        let f = arg(a, c, 0);
        let p = prim_var(a);
        invoke(a, c.site, f, None, vec![elem, p, this]);
      }
    });
  }
  for name in [
    "Array.prototype.slice",
    "Array.prototype.concat",
    "Array.prototype.reverse",
    "Array.prototype.sort",
    "Array.prototype.flat",
    "Array.prototype.toSorted",
    "Array.prototype.toReversed",
    "Array.prototype.with",
    "Array.prototype.filter",
    "Array.prototype.toSpliced",
    "Array.prototype.values",
    "Array.prototype.keys",
    "Array.prototype.entries",
    "Array.prototype.@@iterator",
  ] {
    a.rule(name, |a, c| {
      let this = c_this(a, c);
      let (_, v, elem) = new_array(a, c.site, 8);
      let items = elem_of(a, this);
      let items = a.admitted_values(items);
      a.copy(items, elem);
      // concat/with: arguments may be elements or arrays of them; sort: a comparator.
      for x in args_from(a, c, 0) {
        match x {
          Arg::Var(x) => {
            a.copy(x, elem);
            a.constrain(x, Constraint::ElemOf(elem));
            invoke(a, c.site, x, None, vec![items, items]);
          }
          Arg::Spread(x) => a.constrain(x, Constraint::ElemOf(elem)),
        }
      }
      a.copy(v, c.ret);
    });
  }
  a.rule("Array.prototype.splice", |a, c| {
    let this = c_this(a, c);
    let (_, v, elem) = new_array(a, c.site, 8);
    let items = elem_of(a, this);
    a.copy(items, elem);
    let added = a.var();
    for x in args_from(a, c, 2) {
      match x {
        Arg::Var(x) => a.copy(x, added),
        Arg::Spread(x) => a.constrain(x, Constraint::ElemOf(added)),
      }
    }
    a.constrain(this, Constraint::Store { key: Key::Unknown, src: added });
    a.copy(v, c.ret);
  });
  a.rule("Array.prototype.map", |a, c| {
    let this = c_this(a, c);
    let f = arg(a, c, 0);
    let items = elem_of(a, this);
    let items = a.admitted_values(items);
    let p = prim_var(a);
    let r = invoke(a, c.site, f, None, vec![items, p, this]);
    let r = a.admitted_values(r);
    let (_, v, elem) = new_array(a, c.site, 8);
    a.copy(r, elem);
    a.copy(v, c.ret);
  });
  a.rule("Array.prototype.flatMap", |a, c| {
    let this = c_this(a, c);
    let f = arg(a, c, 0);
    let items = elem_of(a, this);
    let items = a.admitted_values(items);
    let p = prim_var(a);
    let r = invoke(a, c.site, f, None, vec![items, p, this]);
    let (_, v, elem) = new_array(a, c.site, 8);
    let flat = a.var();
    a.copy(r, flat);
    a.constrain(r, Constraint::ElemOf(flat));
    let flat = a.admitted_values(flat);
    a.copy(flat, elem);
    a.copy(v, c.ret);
  });
  for name in [
    "Array.prototype.forEach",
    "Array.prototype.some",
    "Array.prototype.every",
    "Array.prototype.findIndex",
    "Array.prototype.findLastIndex",
  ] {
    a.rule(name, |a, c| {
      let this = c_this(a, c);
      let f = arg(a, c, 0);
      let items = elem_of(a, this);
      let p = prim_var(a);
      invoke(a, c.site, f, None, vec![items, items, this]);
      a.seed(c.ret, a.prim);
      let _ = p;
    });
  }
  for name in ["Array.prototype.reduce", "Array.prototype.reduceRight"] {
    a.rule(name, |a, c| {
      let this = c_this(a, c);
      let f = arg(a, c, 0);
      let items = elem_of(a, this);
      let acc = a.var();
      if c.args.len() > 1 {
        let init = arg(a, c, 1);
        a.copy(init, acc);
      }
      a.copy(items, acc);
      let p = prim_var(a);
      let r = invoke(a, c.site, f, None, vec![acc, items, p, this]);
      a.copy(r, acc);
      a.copy(acc, c.ret);
    });
  }
  a.rule("Array.prototype.join", |a, c| {
    let this = c_this(a, c);
    // Constant strings joined by a constant separator fold to a constant.
    let sep = if c.args.is_empty() {
      Some(",".to_string())
    } else {
      let s = arg(a, c, 0);
      const_string_of(a, s)
    };
    let mut folded = None;
    if let Some(sep) = sep {
      let arrays: Vec<ObjId> = a.pts(this).iter().copied().collect();
      if arrays.len() == 1 {
        let arr = arrays[0];
        if matches!(a.objs[arr].kind, ObjKind::Array) {
          if let Some(elem) = a.objs[arr].elem {
            let parts: Vec<Option<String>> = a
              .pts(elem)
              .iter()
              .map(|o| match &a.objs[*o].kind {
                ObjKind::Str(s) => Some(s.to_string()),
                _ => None,
              })
              .collect();
            if !parts.is_empty() && parts.iter().all(Option::is_some) {
              let strings: Vec<String> = parts.into_iter().flatten().collect();
              let _ = sep;
              // Element order is not tracked; only a single-element or identical-element join is exact.
              if strings.len() == 1 {
                folded = Some(strings[0].clone());
              }
            }
          }
        }
      }
    }
    match folded {
      Some(s) => {
        let o = a.str(&s);
        a.seed(c.ret, o);
      }
      None => {
        let items = elem_of(a, this);
        a.constrain(items, Constraint::Protocol(Protocol::Primitive));
        a.seed(c.ret, a.top_host);
      }
    }
  });
  for name in [
    "Array.prototype.indexOf",
    "Array.prototype.lastIndexOf",
    "Array.prototype.includes",
    "Array.prototype.has",
    "Array.prototype.delete",
    "Array.prototype.clear",
    "Array.prototype.size",
    "Array.prototype.length",
  ] {
    a.rule(name, |a, c| a.seed(c.ret, a.prim));
  }
  // Containers
  for name in ["Map", "Set", "WeakMap", "WeakSet", "WeakRef"] {
    a.rule(name, |a, c| {
      let (_, v, elem) = new_array(a, c.site, 9);
      if !c.args.is_empty() {
        let init = arg(a, c, 0);
        let pairs = elem_of(a, init);
        a.copy(pairs, elem);
        a.constrain(pairs, Constraint::ElemOf(elem));
      }
      a.copy(v, c.ret);
    });
  }
  // Promise
  a.rule("Promise", |a, c| {
    let (_, v, value) = new_promise(a, c.site, 10);
    let executor = arg(a, c, 0);
    let resolve_id = a.rule_id("promise.resolve").unwrap();
    let resolve = a.builtin(resolve_id, Some(value), None);
    let rv = a.var();
    a.seed(rv, resolve);
    let reject = prim_var(a);
    invoke(a, c.site, executor, None, vec![rv, reject]);
    a.copy(v, c.ret);
  });
  a.rule("promise.resolve", |a, c| {
    // The value it settles: the builtin's own data variable (the promise's value, or an adopted
    // thenable's settlement), not the memoised call's copy of it.
    let value = a.builtin_data(c).unwrap_or_else(|| c_data(c));
    let v = arg(a, c, 0);
    // Resolving with a thenable adopts it (`AwaitOf` calls its `then`).
    let settled = await_of(a, v);
    a.copy(settled, value);
  });
  a.rule("Promise.resolve", |a, c| {
    let (_, pv, value) = new_promise(a, c.site, 10);
    let v = arg(a, c, 0);
    let settled = await_of(a, v);
    a.copy(settled, value);
    a.copy(pv, c.ret);
  });
  a.rule("Promise.reject", |a, c| {
    let (_, pv, _) = new_promise(a, c.site, 10);
    a.copy(pv, c.ret);
  });
  for name in ["Promise.all", "Promise.allSettled", "Promise.any"] {
    a.rule(name, |a, c| {
      let (_, pv, value) = new_promise(a, c.site, 10);
      let (_, av, elem) = new_array(a, c.site, 11);
      let first = arg(a, c, 0);
      let items = elem_of(a, first);
      let settled = await_of(a, items);
      a.copy(settled, elem);
      a.copy(av, value);
      a.copy(pv, c.ret);
    });
  }
  a.rule("Promise.race", |a, c| {
    let (_, pv, value) = new_promise(a, c.site, 10);
    let first = arg(a, c, 0);
    let items = elem_of(a, first);
    let settled = await_of(a, items);
    a.copy(settled, value);
    a.copy(pv, c.ret);
  });
  a.rule("Promise.then", |a, c| {
    let this = c_this(a, c);
    let value = await_of(a, this);
    let (_, pv, out) = new_promise(a, c.site, 12);
    if c.args.is_empty() {
      a.copy(value, out);
    } else {
      let f = arg(a, c, 0);
      let r = invoke(a, c.site, f, None, vec![value]);
      let settled = await_of(a, r);
      a.copy(settled, out);
    }
    if c.args.len() > 1 {
      let g = arg(a, c, 1);
      let h = host_var(a);
      let r = invoke(a, c.site, g, None, vec![h]);
      let settled = await_of(a, r);
      a.copy(settled, out);
    }
    a.copy(pv, c.ret);
  });
  a.rule("Promise.catch", |a, c| {
    let this = c_this(a, c);
    let value = await_of(a, this);
    let (_, pv, out) = new_promise(a, c.site, 12);
    a.copy(value, out);
    if !c.args.is_empty() {
      let g = arg(a, c, 0);
      let h = host_var(a);
      let r = invoke(a, c.site, g, None, vec![h]);
      let settled = await_of(a, r);
      a.copy(settled, out);
    }
    a.copy(pv, c.ret);
  });
  a.rule("Promise.finally", |a, c| {
    // The callback's result is awaited (a thenable is adopted, so its `then` runs), and the
    // promise settles with its original value: the result is not used.
    let this = c_this(a, c);
    if !c.args.is_empty() {
      let f = arg(a, c, 0);
      let r = invoke(a, c.site, f, None, vec![]);
      await_of(a, r);
    }
    a.copy(this, c.ret);
  });

  // JSON, String, Number, Symbol, console
  a.rule("JSON.stringify", |a, c| {
    let v = arg(a, c, 0);
    a.constrain(v, Constraint::Protocol(Protocol::Json));
    if c.args.len() > 1 {
      let replacer = arg(a, c, 1);
      let h = host_var(a);
      invoke(a, c.site, replacer, None, vec![h, v]);
    }
    a.seed(c.ret, a.top_host);
  });
  a.rule("JSON.parse", |a, c| a.seed(c.ret, a.top_host));
  for name in [
    "String",
    "Number",
    "Boolean",
    "BigInt",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "String.prototype.concat",
  ] {
    a.rule(name, |a, c| {
      for x in args_from(a, c, 0) {
        if let Arg::Var(x) = x {
          a.constrain(x, Constraint::Protocol(Protocol::Primitive));
        }
      }
      a.seed(c.ret, a.top_host);
    });
  }
  a.rule("Symbol", |a, c| {
    let name = format!("@@sym:{}:{}", c.site.module.raw(), c.site.start);
    let o = a.str(&name);
    a.seed(c.ret, o);
  });
  a.rule("Symbol.for", |a, c| {
    let k = arg(a, c, 0);
    match const_string_of(a, k) {
      Some(s) => {
        let o = a.str(&format!("@@for:{s}"));
        a.seed(c.ret, o);
      }
      None => a.seed(c.ret, a.top_host),
    }
  });
  for name in [
    "console.log",
    "console.info",
    "console.warn",
    "console.error",
    "console.debug",
    "console.trace",
    "console.dir",
    "console.table",
    "console.group",
    "util.inspect",
    "util.format",
    "util.formatWithOptions",
    "console.assert",
  ] {
    a.rule(name, |a, c| {
      for x in args_from(a, c, 0) {
        match x {
          Arg::Var(x) => a.constrain(x, Constraint::Protocol(Protocol::Inspect)),
          Arg::Spread(x) => {
            let e = elem_of(a, x);
            a.constrain(e, Constraint::Protocol(Protocol::Inspect));
          }
        }
      }
      a.seed(c.ret, a.top_host);
    });
  }
  a.rule("structuredClone", |a, c| a.seed(c.ret, a.top_host));
  for name in [
    "setTimeout",
    "setInterval",
    "setImmediate",
    "queueMicrotask",
    "requestAnimationFrame",
    "requestIdleCallback",
    "process.nextTick",
  ] {
    a.rule(name, |a, c| {
      let f = arg(a, c, 0);
      let rest: Vec<Arg> = args_from(a, c, 2);
      invoke_args(a, c.site, f, None, rest);
      a.seed(c.ret, a.top_host);
    });
  }
  for name in [
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "ReferenceError",
    "EvalError",
    "URIError",
    "AggregateError",
    "DOMException",
  ] {
    a.rule(name, |a, c| {
      let (obj, v) = new_object(a, c.site, 13);
      let message = arg(a, c, 0);
      let mf = a.field(obj, &ArcStr::from("message"));
      a.copy(message, mf);
      a.constrain(message, Constraint::Protocol(Protocol::Primitive));
      if c.args.len() > 1 {
        let options = arg(a, c, 1);
        let cause = load(a, options, "cause");
        let cf = a.field(obj, &ArcStr::from("cause"));
        a.copy(cause, cf);
      }
      let stack = a.field(obj, &ArcStr::from("stack"));
      a.seed(stack, a.top_host);
      a.copy(v, c.ret);
    });
  }
  a.rule("Proxy", |a, c| {
    let target = arg(a, c, 0);
    let handler = arg(a, c, 1);
    escape(a, target, Flavour::Host);
    escape(a, handler, Flavour::Host);
    a.copy(target, c.ret);
  });
  a.rule("process.getBuiltinModule", |a, c| {
    let name = arg(a, c, 0);
    match const_string_of(a, name) {
      Some(s) => {
        let m = external_module(a, &s);
        a.seed(c.ret, m);
      }
      None => a.seed(c.ret, a.top_ext),
    }
  });
  for name in ["Iterator.next", "Iterator.return", "Iterator.throw"] {
    a.rule(name, |a, c| {
      let this = this_of(a, c);
      let (obj, v) = new_object(a, c.site, 30);
      let value = a.field(obj, &ArcStr::from("value"));
      let done = a.field(obj, &ArcStr::from("done"));
      a.seed(done, a.prim);
      let iters: Vec<ObjId> = a.pts(this).iter().copied().collect();
      for it in iters {
        if let ObjKind::Iterator(f) = a.objs[it].kind {
          a.activate(f);
          let y = a.fns[f].yielded;
          a.copy(y, value);
          let r = a.fns[f].ret;
          a.copy(r, value);
        }
      }
      a.copy(v, c.ret);
    });
  }
  for name in ["Iterator.@@iterator", "Iterator.@@asyncIterator"] {
    a.rule(name, |a, c| {
      let this = this_of(a, c);
      a.copy(this, c.ret);
    });
  }
  a.rule("host.opaque", |a, c| {
    a.escape_args(c, Flavour::Host);
    a.seed(c.ret, a.top_host);
  });
  a.rule("host.prim", |a, c| a.seed(c.ret, a.prim));
  a.rule("host.identity", |a, c| {
    let v = arg(a, c, 0);
    a.copy(v, c.ret);
  });
  a.rule("String.prototype.toString", |a, c| {
    let this = c_this(a, c);
    a.copy(this, c.ret);
  });
}

fn c_this(a: &mut Analysis, c: &Call) -> VarId {
  this_of(a, c)
}

fn c_data(c: &Call) -> VarId {
  // Rules with data are reached through `Builtin { data: Some(..) }`; the heap passes the data
  // variable as `this` when the builtin has no receiver of its own.
  c.this.expect("builtin data")
}

pub fn const_string_of(a: &Analysis, v: VarId) -> Option<String> {
  let mut out = None;
  for o in a.pts(v) {
    match &a.objs[*o].kind {
      ObjKind::Str(s) => {
        if out.is_some() {
          return None;
        }
        out = Some(s.to_string());
      }
      _ => return None,
    }
  }
  out
}

/// Global objects: a table of (name, members). A member rule name is `<global>.<member>`.
const GLOBAL_OBJECTS: &[(&str, &[&str])] = &[
  (
    "Object",
    &[
      "keys",
      "values",
      "entries",
      "assign",
      "freeze",
      "seal",
      "preventExtensions",
      "create",
      "getPrototypeOf",
      "setPrototypeOf",
      "defineProperty",
      "defineProperties",
      "fromEntries",
      "hasOwn",
      "getOwnPropertyDescriptor",
      "getOwnPropertyDescriptors",
      "getOwnPropertyNames",
      "getOwnPropertySymbols",
      "is",
    ],
  ),
  (
    "Reflect",
    &[
      "get",
      "set",
      "has",
      "ownKeys",
      "apply",
      "construct",
      "getPrototypeOf",
      "setPrototypeOf",
      "defineProperty",
      "deleteProperty",
      "getOwnPropertyDescriptor",
    ],
  ),
  ("Array", &["isArray", "from", "of"]),
  ("Promise", &["resolve", "reject", "all", "allSettled", "any", "race"]),
  ("JSON", &["stringify", "parse"]),
  ("Symbol", &["for"]),
  (
    "console",
    &["log", "info", "warn", "error", "debug", "trace", "dir", "table", "group", "assert"],
  ),
];

const WELL_KNOWN_SYMBOLS: &[&str] = &[
  "iterator",
  "asyncIterator",
  "hasInstance",
  "toPrimitive",
  "toStringTag",
  "species",
  "isConcatSpreadable",
  "match",
  "replace",
  "search",
  "split",
  "unscopables",
  "dispose",
  "asyncDispose",
];

const CALLABLE_GLOBALS: &[&str] = &[
  "Object",
  "Array",
  "Map",
  "Set",
  "WeakMap",
  "WeakSet",
  "WeakRef",
  "Promise",
  "String",
  "Number",
  "Boolean",
  "BigInt",
  "Symbol",
  "parseInt",
  "parseFloat",
  "isNaN",
  "isFinite",
  "structuredClone",
  "setTimeout",
  "setInterval",
  "setImmediate",
  "queueMicrotask",
  "requestAnimationFrame",
  "requestIdleCallback",
  "Error",
  "TypeError",
  "RangeError",
  "SyntaxError",
  "ReferenceError",
  "EvalError",
  "URIError",
  "AggregateError",
  "DOMException",
  "Proxy",
];

const PRIM_GLOBALS: &[&str] = &[
  "Math",
  "Date",
  "performance",
  "Intl",
  "Atomics",
  "escape",
  "unescape",
  "encodeURIComponent",
  "decodeURIComponent",
  "encodeURI",
  "decodeURI",
  "clearTimeout",
  "clearInterval",
  "clearImmediate",
  "cancelAnimationFrame",
];

/// Resolves a free global name to a variable holding its model.
pub fn resolve_global(
  a: &mut Analysis,
  globals: &mut FxHashMap<ArcStr, VarId>,
  name: &str,
) -> VarId {
  if let Some(v) = globals.get(name) {
    return *v;
  }
  let v = a.var();
  globals.insert(ArcStr::from(name), v);
  let obj = global_object(a, name);
  a.seed(v, obj);
  v
}

fn global_object(a: &mut Analysis, name: &str) -> ObjId {
  if let Some((_, members)) = GLOBAL_OBJECTS.iter().find(|(n, _)| *n == name) {
    let callable = CALLABLE_GLOBALS.contains(&name);
    let obj = if callable {
      let id = a.rule_id(name).unwrap_or_else(|| a.rule_id("host.opaque").unwrap());
      let obj = a.alloc(
        ObjKind::Builtin { rule: id, data: None, arity: None, this: None, target: None },
        None,
      );
      builtin_prototype(a, obj);
      obj
    } else {
      a.alloc(ObjKind::Object, None)
    };
    for m in *members {
      let full = format!("{name}.{m}");
      let id = a.rule_id(&full).unwrap_or_else(|| a.rule_id("host.opaque").unwrap());
      let b = a.builtin(id, None, None);
      let f = a.field(obj, &ArcStr::from(*m));
      a.seed(f, b);
    }
    if name == "Symbol" {
      for s in WELL_KNOWN_SYMBOLS {
        let o = a.str(&format!("@@{s}"));
        let f = a.field(obj, &ArcStr::from(*s));
        a.seed(f, o);
      }
    }
    if name == "console" {
      // `console` is a host object: unknown members behave like `log`.
      let elem = a.elem(obj);
      let id = a.rule_id("console.log").unwrap();
      let b = a.builtin(id, None, None);
      a.seed(elem, b);
    } else {
      let elem = a.elem(obj);
      a.seed(elem, a.top_host);
    }
    return obj;
  }
  if CALLABLE_GLOBALS.contains(&name) {
    let id = a.rule_id(name).unwrap_or_else(|| a.rule_id("host.opaque").unwrap());
    let obj = a.alloc(
      ObjKind::Builtin { rule: id, data: None, arity: None, this: None, target: None },
      None,
    );
    builtin_prototype(a, obj);
    return obj;
  }
  if PRIM_GLOBALS.contains(&name) {
    // Pure computations on primitives: members are opaque host functions returning host data.
    return a.alloc(ObjKind::External { flavour: Flavour::Host, name: ArcStr::from(name) }, None);
  }
  match name {
    "globalThis" | "window" | "self" | "global" => {
      let obj = a.alloc(ObjKind::Object, None);
      // Anything stored on the global object is visible to code the pass cannot see.
      a.objs[obj].escaped = Some(Flavour::Host);
      let elem = a.elem(obj);
      a.seed(elem, a.top_host);
      obj
    }
    "require" | "module" | "exports" | "__dirname" | "__filename" | "Function" | "eval" => {
      a.alloc(ObjKind::External { flavour: Flavour::Ext, name: ArcStr::from(name) }, None)
    }
    "process" => {
      a.alloc(ObjKind::External { flavour: Flavour::Host, name: ArcStr::from("process") }, None)
    }
    _ => a.alloc(ObjKind::External { flavour: Flavour::Host, name: ArcStr::from(name) }, None),
  }
}

/// `X.prototype` of a host constructor: writing to it invalidates the builtin models.
fn builtin_prototype(a: &mut Analysis, ctor: ObjId) {
  let proto = a.alloc(ObjKind::Object, None);
  a.objs[proto].builtin_proto = true;
  let f = a.field(ctor, &ArcStr::from("prototype"));
  a.seed(f, proto);
}

/// The namespace object of an external module.
pub fn external_module(a: &mut Analysis, specifier: &str) -> ObjId {
  let name = specifier.strip_prefix("node:").unwrap_or(specifier);
  // Code loaders hand what they receive to code built at run time: external, not host.
  let loader =
    matches!(name, "vm" | "module" | "worker_threads" | "child_process" | "inspector" | "repl");
  let flavour = if is_node_builtin(specifier) && !loader { Flavour::Host } else { Flavour::Ext };
  let obj = a.alloc(ObjKind::External { flavour, name: ArcStr::from(name) }, None);
  if name == "util" {
    // `util.inspect` and `util.inspect.custom`.
    let id = a.rule_id("util.inspect").unwrap();
    let inspect = a.builtin(id, None, None);
    let custom = a.str(INSPECT_CUSTOM);
    let cf = a.field(inspect, &ArcStr::from("custom"));
    a.seed(cf, custom);
    let f = a.field(obj, &ArcStr::from("inspect"));
    a.seed(f, inspect);
    let id = a.rule_id("util.format").unwrap();
    let format = a.builtin(id, None, None);
    let f = a.field(obj, &ArcStr::from("format"));
    a.seed(f, format);
  }
  obj
}
