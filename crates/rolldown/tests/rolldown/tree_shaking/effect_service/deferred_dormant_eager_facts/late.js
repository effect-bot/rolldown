// Reached only once the service's `factsUsed` result is called.
export const reachedLate = () => lateHelper();

export class LateClass {
  lateMethod() {
    return 'late';
  }
}

const lateHelper = () => new LateClass().lateMethod();
