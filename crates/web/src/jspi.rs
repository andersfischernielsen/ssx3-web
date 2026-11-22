//! Guest threads on JSPI (JavaScript Promise Integration): the engine's
//! thread layer (`ssx3_ps2::threads`) asks for one stack per guest thread and
//! a switch between them; here every guest thread is a `WebAssembly.promising`
//! call of [`jspi_thread_entry`], parked by a `WebAssembly.Suspending` import
//! ([`jspi_switch`]) until the baton comes back (`js/jspi.js`).
//!
//! The wasm stacks are the engine's; the shadow stack (Rust's linear-memory
//! stack, one global `__stack_pointer`) is ours: each guest thread gets its own
//! block of memory, and `js/jspi.js` swaps the pointer at every switch.

use std::rc::Rc;

use ssx3_engine::ssx3_ps2::threads::{self, StackSwitcher};
use wasm_bindgen::prelude::*;

/// Shadow stack per guest thread. The frame driver (token 0) keeps the
/// module's own, bigger one (`-zstack-size` in `build.sh`).
const SHADOW_STACK: usize = 8 << 20;

#[wasm_bindgen(module = "/js/jspi.js")]
extern "C" {
    /// A guest thread was created: its shadow stack ends at `top`.
    fn jspi_spawn(id: i32, top: u32);
    /// Hand the baton from `me` to `to`; returns when it comes back. The
    /// worker wraps this import in `WebAssembly.Suspending` (the promise it
    /// returns is what parks the stack), so the declared type is a lie.
    fn jspi_switch(me: i32, to: i32) -> i32;
}

struct Switcher;

impl StackSwitcher for Switcher {
    fn spawn(&self, id: i32) {
        // Never freed: a parked stack stays parked (see `threads_switcher.rs`).
        let block: &'static mut [u8] = Box::leak(vec![0u8; SHADOW_STACK].into_boxed_slice());
        let top = (block.as_ptr() as usize + block.len()) & !15;
        jspi_spawn(id, top as u32);
    }

    fn switch(&self, me: i32, to: i32) {
        jspi_switch(me, to);
    }
}

/// Install the stack switcher; call before the first frame.
pub fn install() {
    threads::set_switcher(Rc::new(Switcher));
}

/// The body of a guest thread, called by `js/jspi.js` through
/// `WebAssembly.promising` on a stack of its own.
#[wasm_bindgen]
pub fn jspi_thread_entry(id: i32) {
    threads::thread_main(id);
}
