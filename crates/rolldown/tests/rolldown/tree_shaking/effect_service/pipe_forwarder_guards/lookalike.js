// Not Effect's pipeArguments: it pipes the receiver's `inner` value.
function pipeArguments(self, args) {
  return args[0](self.inner);
}

export const lookalike = (inner) => ({
  inner,
  pipe() {
    return pipeArguments(this, arguments);
  },
});
