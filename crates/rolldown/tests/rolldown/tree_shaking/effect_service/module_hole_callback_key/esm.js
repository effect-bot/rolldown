// None of these helpers imports `effect`: each is a module hole until a service, a
// key or a provider reaches it. Each names the key itself, at run time.
export function run(callback) {
  return callback('test/ModuleHoleCallbackKey');
}
