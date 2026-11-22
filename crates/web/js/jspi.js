// Guest threads on JSPI (crates/web/src/jspi.rs): one suspended wasm stack
// per guest thread. A thread is a `WebAssembly.promising` call of
// `jspi_thread_entry`; `jspi_switch` (a `WebAssembly.Suspending` import: the
// worker wraps it, `bind` below) parks the calling stack on a promise that
// the next switch to it resolves. The frame driver (token 0) is a promising
// call too (`Game.frame`, started by the worker).
//
// Rust's shadow stack (one global `__stack_pointer`) is swapped at every
// switch: each thread has its own block of memory (`jspi_spawn`).

const j = {
  sp: null,
  entry: null,
  onError: null,
  stackTop: new Map(),
  // Tokens whose stack exists (the frame driver's, 0, from the start).
  started: new Set([0]),
  waiting: new Map(),
  saved: new Map(),
};

// Called by worker.js with the instance's exports, before the first frame.
globalThis.__ssx3_jspi = {
  bind(exports, onError) {
    j.sp = exports.__stack_pointer;
    j.entry = WebAssembly.promising(exports.jspi_thread_entry);
    j.onError = onError;
  },
};

export function jspi_spawn(id, top) {
  j.stackTop.set(id, top);
}

export function jspi_switch(me, to) {
  j.saved.set(me, j.sp.value);
  const parked = new Promise((resolve) => j.waiting.set(me, resolve));
  if (j.started.has(to)) {
    const resume = j.waiting.get(to);
    if (!resume) throw new Error(`guest thread ${to} is not parked`);
    j.waiting.delete(to);
    resume();
  } else {
    j.started.add(to);
    j.sp.value = j.stackTop.get(to);
    // Runs on its own stack until its first switch; a trap rejects.
    j.entry(to).catch(j.onError);
  }
  return parked.then(() => {
    j.sp.value = j.saved.get(me);
    return 0;
  });
}
