//! The browser build's VU helper (`www/vuworker.js`): the
//! renderer's chunks of VU1 calls run here, in parallel, as they run on
//! threads natively. Jobs and results are bytes (`ssx3_render::vujob`);
//! replayed calls' shared input comes from the store the game worker keeps
//! in the shared buffer, mirrored here (`store_reset`, `store_append`).
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;

use wasm_bindgen::prelude::*;

// Talc instead of the default dlmalloc: allocation and freeing are a few
// percent of a frame, and talc is the faster of the two in wasm.
#[global_allocator]
static TALC: talc::wasm::WasmDynamicTalc = talc::wasm::new_wasm_dynamic_allocator();

thread_local! {
    static STORE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// The store started over (a new generation).
#[wasm_bindgen]
pub fn store_reset() {
    STORE.with_borrow_mut(|s| s.clear());
}

/// The store grew by `bytes`.
#[wasm_bindgen]
pub fn store_append(bytes: &[u8]) {
    STORE.with_borrow_mut(|s| s.extend_from_slice(bytes));
}

/// Run one encoded job; returns the encoded result.
#[wasm_bindgen]
pub fn run_job(job: &[u8]) -> Vec<u8> {
    STORE.with_borrow(|s| ssx3_render::vujob::run(job, s))
}
