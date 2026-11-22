# ssx3-web: SSX 3 in the browser

The recompiled game ([ssx3-decomp](https://github.com/andersfischernielsen/ssx3-decomp))
as WebAssembly, drawn with WebGPU. Experimental.

This is a front end of the game the way any other is: it uses `ssx3-engine`
from a git submodule (`ssx3/`) and only its documented API
(`ssx3/docs/frontends.md`). What it needs from the engine beyond that goes
into `ssx3-engine` there.

## Setup

You need your own copy of the game: the generated code is made locally
from your disc image and is never committed. Then:

```sh
git clone --recursive git@github.com:andersfischernielsen/ssx3-web.git
cd ssx3-web/ssx3
python3 tools/setup.py --iso /path/to/"SSX 3 (USA).iso"   # orig/, generated/ (ee, iop, vu)
cd ..
cargo install wasm-bindgen-cli --version 0.2.129 --locked   # once; must match Cargo.lock
sh build.sh && python3 serve.py   # http://localhost:8000, choose the ISO in the page
```

Open the page in a browser with WebGPU, for example Chrome or Edge 113+, or
Safari 26+. Then choose the `SSX 3 (USA)` ISO.

Rust stable with the `wasm32-unknown-unknown` target
(`rustup target add wasm32-unknown-unknown`) is needed.

To update the game: bump the submodule (`git -C ssx3 pull`, commit the new
pointer), then run `python3 tools/setup.py` in `ssx3/` again. Before committing the bump, smoke-test the browser
(`AGENTS.md`, "Checking a change").

| Path | What it is |
| --- | --- |
| `crates/web` | the front end (`ssx3-web`, cdylib): boot, canvas blit, VU helper pool, audio pull, pause |
| `crates/vuworker` | the VU helper (`ssx3-vuworker`, cdylib), run by `www/vuworker.js` in helper workers |
| `www/` | the page and the workers; `pkg/` and `pkg-vu/` are build output |
| `build.sh` | the wasm32 build (32 MiB stack, exported stack pointer, simd128) and `wasm-bindgen` |
| `serve.py` | a local server with the cross-origin isolation headers (`--lan` for HTTPS) |
| `ssx3/` | `ssx3-decomp`, pinned |

## How it works

- **EE code and guest threads.** The browser runs the same generated code as
  every other front end. The game is written as EE kernel threads, each on a
  stack of its own; here the stacks are WebAssembly JSPI (JavaScript Promise
  Integration) stacks. A guest thread is a `WebAssembly.promising` call of
  `jspi_thread_entry`; a switch is a `WebAssembly.Suspending` import
  (`jspi_switch`) that parks the calling stack on a promise the next switch
  resolves (`crates/web/js/jspi.js`, `crates/web/src/jspi.rs`, the engine's
  `ssx3_ps2::threads::StackSwitcher`). The game frame itself is a promising
  call the worker awaits (`www/worker.js`). Rust's shadow stack pointer
  (`__stack_pointer`, exported by `build.sh`) is swapped at every switch: each
  guest thread has its own block of memory. Needs JSPI: Chrome and Edge 137+,
  Firefox 153+, Safari 27+.
- **Worker.** The game runs in a Web Worker (`www/worker.js`), on a
  `requestAnimationFrame` loop paced to 59.94 game frames per second.
- **Disc image.** The page hands over the disc image as a `File`. The worker
  reads byte ranges from it synchronously with `FileReaderSync`, which only
  exists in workers, through the engine's `ssx3_core::iso::set_opener` hook. The
  executable comes from the disc as well. Nothing is uploaded.
- **Rendering.** `ssx3-gfx` draws into an `OffscreenCanvas` with WebGPU, at
  the canvas's pixel size. With `cfg(target_arch = "wasm32")`, the renderer
  and machine do the following without threads or blocking:
  - the VU1 calls run inline;
  - the frame snapshot is a single copy;
  - the GS has no MTGS thread;
  - the light-occlusion depth readback is used only once it is mapped;
  - the device is created with `gpu::init_context().await`.
- **Input.** Keyboard only, with the native window's keys:
  - arrows: D-pad;
  - Enter: Start;
  - Right Shift: Select;
  - X: Cross, S: Circle, Z: Square, A: Triangle;
  - Q: L1, W: R1, 1: L2, 2: R2.

- **VU helpers.** The renderer's VU1 calls, the native VU programs of
  `ssx3-render`, run in chunks on helper workers (`www/vuworker.js`,
  `crates/vuworker`, a 180 KB module), as they run on threads natively
  (`ssx3_engine::vu`; "VU offload" in `ssx3/docs/frontends.md`).
  - Jobs and results are bytes (`ssx3_render::vujob`), passed through a
    SharedArrayBuffer job queue.
  - The game worker takes jobs too, then waits with `Atomics.wait`.
  - This needs cross-origin isolation, which `serve.py` sends the headers
    for. Without it, everything runs in the game worker.
  - `SSX3_VU_OFFLOAD_SELFTEST=1` on the native `ssx3-headless` runs every
    chunk through the same encode/run/decode path; the screenshots are
    identical.

- **Sound.** The native mixer's 48 kHz stereo goes from the worker to the
  page each frame and plays through an AudioWorklet (`www/audio.js`), which
  buffers at most ~200 ms.
- **Hidden page.** While the page is hidden the game pauses (the worker's
  frame loop stops, the engine's sound is silenced, the `AudioContext` is
  suspended); it resumes where it was when the page is shown again.
- **Settings.** `SSX3_*` keys in the page's URL query are the game's
  settings (`ssx3/docs/options.md`), e.g. `?SSX3_TICK_HZ=120`;
  `?SSX3_EVENT=ARA1` boots straight into a race. Page options: `?fps=1`
  shows an fps counter over the canvas (off by default: in Firefox, updating
  the page over the canvas makes it now and then show an older canvas frame,
  a visible jump back), `?SSX3_PRESENT_MARKER=1` draws the present count and
  the game frame as bit cells in the image's bottom-left corner (to check
  frame order in a screen recording), `?SSX3_VU_STORE_MB=1` shrinks the VU
  helpers' store of shared input from 24 MB (it then starts over every few
  frames: a check of that path). A crash logs its stack to the console
  (`[ssx3] the game crashed` or `[ssx3] VU helper N failed`), and the wasm
  memory size is logged every 10 s.
- **Image.** The game's 4:3 view boxed in the canvas, rendered at the box's
  pixel size (the render scale costs little: the time goes to the CPU
  side), no anisotropic filtering. `?SSX3_ASPECT=16:9` boxes at 16:9,
  `?SSX3_ASPECT=fill` fills the canvas (the view widened to it).

Not yet supported: the memory card, gamepads, the settings overlay,
interpolation and texture packs.

## Performance (headless Chrome, M-series Mac)

- **Menus and front end:** 60 fps.
- **Races:** about 55-60 fps once under way (17 ms per frame), about 33 fps
  in the heaviest stretch at the start. Per race frame, on the game worker:
  - about half is game code;
  - the renderer: building the typed frame, the GS draws from the VU1
    output and the WebGPU calls, a few ms each.
- **Frame staging.** A frame's VU1 chunks start on the helper workers when
  the game publishes it, and the frame is drawn when the next one is
  published (`Inner::publish_staged`). The game's next frame runs while the
  helpers work, which hides the largest chunk (11-17 ms; a run of draws of
  one program that pass state through VU memory cannot be split). One frame
  of latency, as with the native render thread.
- **Light queries.** The depth read back for the lens-flare queries is
  converted per pixel when a query reads it, not as a whole every frame.

Next steps, roughly by payoff:
- split that chunk. Measured, by running each call from the true memory and
  from the inputs-only image a split would start from:
  - the rider program's drawing calls all read the bone matrices its setup
    call computes;
  - the world-mesh program's second pass (0x22c8) reads the first pass's
    output, which is fine since both are in one draw;
  - but 1.6% of its first passes (0x2270) also read state an earlier call
    wrote (probably the fan tag toggle at qw33.w).

  So the world-mesh runs could be split if that state is identified and
  seeded at each split;
- run the game and the renderer in different workers;
- a wasm-SIMD version of the PS2 float helpers (the NEON path needs
  per-lane shifts and count-leading-zeros, which wasm SIMD lacks).

## Checking the game natively

The browser runs the code every front end runs, so `ssx3-decomp`'s race replay
gate covers the game (`ssx3/AGENTS.md`). What is specific to the browser is the
thread switching and the frame loop: smoke-test them in a browser (race,
`?SSX3_EVENT=ARA1`, for a minute or more).

`SSX3_VU_OFFLOAD_SELFTEST=1` on the native `ssx3-headless` runs every
    chunk through the same encode/run/decode path; the screenshots are
    identical.

- **Sound.** The native mixer's 48 kHz stereo goes from the worker to the
  page each frame and plays through an AudioWorklet (`www/audio.js`), which
  buffers at most ~200 ms.
- **Hidden page.** While the page is hidden the game pauses (the worker's
  frame loop stops, the engine's sound is silenced, the `AudioContext` is
  suspended); it resumes where it was when the page is shown again.
- **Settings.** `SSX3_*` keys in the page's URL query are the game's
  settings (`ssx3/docs/options.md`), e.g. `?SSX3_TICK_HZ=120`;
  `?SSX3_EVENT=ARA1` boots straight into a race. Page options: `?fps=1`
  shows an fps counter over the canvas (off by default: in Firefox, updating
  the page over the canvas makes it now and then show an older canvas frame,
  a visible jump back), `?SSX3_PRESENT_MARKER=1` draws the present count and
  the game frame as bit cells in the image's bottom-left corner (to check
  frame order in a screen recording), `?SSX3_VU_STORE_MB=1` shrinks the VU
  helpers' store of shared input from 24 MB (it then starts over every few
  frames: a check of that path). A crash logs its stack to the console
  (`[ssx3] the game crashed` or `[ssx3] VU helper N failed`), and the wasm
  memory size is logged every 10 s.
- **Image.** The game's 4:3 view boxed in the canvas, rendered at the box's
  pixel size (the render scale costs little: the time goes to the CPU
  side), no anisotropic filtering. `?SSX3_ASPECT=16:9` boxes at 16:9,
  `?SSX3_ASPECT=fill` fills the canvas (the view widened to it).

Not yet supported: the memory card, gamepads, the settings overlay,
interpolation and texture packs.

## Performance (headless Chrome, M-series Mac)

- **Menus and front end:** 60 fps.
- **Races:** about 55-60 fps once under way (17 ms per frame), about 33 fps
  in the heaviest stretch at the start. Per race frame, on the game worker:
  - about half is game code;
  - the renderer: building the typed frame, the GS draws from the VU1
    output and the WebGPU calls, a few ms each.
- **Frame staging.** A frame's VU1 chunks start on the helper workers when
  the game publishes it, and the frame is drawn when the next one is
  published (`Inner::publish_staged`). The game's next frame runs while the
  helpers work, which hides the largest chunk (11-17 ms; a run of draws of
  one program that pass state through VU memory cannot be split). One frame
  of latency, as with the native render thread.
- **Light queries.** The depth read back for the lens-flare queries is
  converted per pixel when a query reads it, not as a whole every frame.

Next steps, roughly by payoff:
- split that chunk. Measured, by running each call from the true memory and
  from the inputs-only image a split would start from:
  - the rider program's drawing calls all read the bone matrices its setup
    call computes;
  - the world-mesh program's second pass (0x22c8) reads the first pass's
    output, which is fine since both are in one draw;
  - but 1.6% of its first passes (0x2270) also read state an earlier call
    wrote (probably the fan tag toggle at qw33.w).

  So the world-mesh runs could be split if that state is identified and
  seeded at each split;
- run the game and the renderer in different workers;
- a wasm-SIMD version of the PS2 float helpers (the NEON path needs
  per-lane shifts and count-leading-zeros, which wasm SIMD lacks).

## Checking the code form natively

The browser runs the unwind form of the EE code. To run that form natively,
without a browser (for example a race replay against `ssx3-decomp`'s RAM
baseline), in `ssx3/`:

```sh
CARGO_TARGET_DIR=target-unwind cargo build --profile game -p ssx3 --bin ssx3-headless --no-default-features --features ee-unwind
SSX3_RAM_DUMP=unwind.bin SSX3_INPUT="$(python3 tools/integrate_wave.py race-input)" target-unwind/game/ssx3-headless 6000
cmp unwind.bin target/gate/base/ds1.bin
```

No halt, empty `misses` and identical RAM mean the form the browser runs
matches the game. `SSX3_VU_OFFLOAD_SELFTEST=1` on the native headless runner checks the
VU offload path (encode, run, decode).

## Checks

CI lints and formats the page and workers with
[oxlint and oxfmt](https://oxc.rs/) (`npm ci`, `npm run lint`,
`npm run fmt:check`; `npm run fmt` formats), checks the Rust formatting, and
check-builds the VU helper for wasm32 against stub generated code. The front
end needs the generated game, so it is built locally (`sh build.sh`).

## No game material

Never commit the ISO, `orig/`, anything under `generated/`, the ELF
(`SLUS_207.72`), RAM or VRAM dumps (`*.bin`) or the wasm builds
(`www/pkg*`: they contain the compiled game code).
