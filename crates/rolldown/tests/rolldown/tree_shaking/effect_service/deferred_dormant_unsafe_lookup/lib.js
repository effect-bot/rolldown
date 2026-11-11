import * as Context from 'effect/Context';

export const Store = Context.Service('test/DeferredDormantUnsafeLookup');

// Nothing reaches this class. Its body could reach any service by key string.
export class UnreachedLookup {
  constructor(context) {
    this.context = context;
  }
  read() {
    return Context.getOrUndefinedUnsafe(this.context, 'test/DeferredDormantUnsafeLookup');
  }
}
