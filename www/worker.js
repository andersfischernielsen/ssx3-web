// The game runs here: FileReaderSync (synchronous disc reads) exists only
// in workers, and a long game frame never blocks the page.
import init, { start } from "./pkg/ssx3_web.js";

const FIELD_MS = 1000 / 59.94;
let game = null;
let buttons = 0;
let size = null;
// The page is hidden: no frames run (the engine is paused, its sound silent).
let paused = false;
let stopped = false;
let raf = 0;
// The module's exports (its memory, for the reports).
let wasm = null;
const mb = () => Math.round(wasm.memory.buffer.byteLength / 2 ** 20);
// `Game.frame` as a promising call (the frame driver is a JSPI stack: guest
// threads switch by parking stacks, js/jspi.js), and the frame in flight, if any.
let promisingFrame = null;
let inflight = null;

// Instantiate the module with its thread-switch import wrapped in
// `WebAssembly.Suspending` and the exports that start a stack wrapped in
// `WebAssembly.promising` (wasm-bindgen's glue cannot do either).
async function load() {
  if (!("Suspending" in WebAssembly)) {
    throw new Error(
      "This browser has no WebAssembly JSPI (JavaScript Promise Integration): " +
        "use Chrome or Edge 137+, Firefox 153+ or Safari 27+.",
    );
  }
  const module = await WebAssembly.compileStreaming(fetch(new URL("./pkg/ssx3_web_bg.wasm", import.meta.url)));
  const instantiate = WebAssembly.instantiate;
  let exports = null;
  WebAssembly.instantiate = async (m, imports) => {
    WebAssembly.instantiate = instantiate;
    // wasm-bindgen imports the snippet's functions as the module namespace.
    for (const [k, ns] of Object.entries(imports)) {
      if (typeof ns.jspi_switch === "function") {
        imports[k] = { ...ns, jspi_switch: new WebAssembly.Suspending(ns.jspi_switch) };
      }
    }
    const instance = await instantiate.call(WebAssembly, m, imports);
    exports = instance.exports;
    return instance;
  };
  await init({ module_or_path: module });
  globalThis.__ssx3_jspi.bind(exports, (err) => crash(err));
  promisingFrame = WebAssembly.promising(exports.game_frame);
  return exports;
}

function crash(err) {
  // A trap (a panic has printed its message just before) or an allocation
  // failure (no message): the stack says where.
  stopped = true;
  const message = `the game crashed (memory ${mb()} MB):\n${err.stack || err}`;
  console.error(`[ssx3] ${message}`);
  postMessage({ type: "error", message });
}

onmessage = async (e) => {
  const m = e.data;
  if (m.type === "start") {
    try {
      wasm = await load();
      // VU helpers: the renderer's VU1 work in parallel (needs a shared
      // buffer, i.e. cross-origin isolation).
      // (`?helpers=N` on the page overrides the count.)
      const auto = self.crossOriginIsolated
        ? Math.max(1, Math.min(6, (navigator.hardwareConcurrency || 4) - 2))
        : 0;
      const helpers = self.crossOriginIsolated && m.helpers != null ? m.helpers : auto;
      game = await start(m.canvas, m.iso, helpers, m.settings);
      const sab = game.vu_buffer();
      for (let id = 0; sab && id < helpers; id++) {
        new Worker("./vuworker.js", { type: "module" }).postMessage({ sab, id });
      }
      postMessage({ type: "running" });
      if (paused) game.pause();
      else loop();
    } catch (err) {
      postMessage({ type: "error", message: String((err && err.stack) || err) });
    }
  } else if (m.type === "buttons") {
    buttons = m.buttons;
  } else if (m.type === "resize") {
    size = [m.width, m.height];
  } else if (m.type === "pause") {
    if (paused) return;
    paused = true;
    cancelAnimationFrame(raf);
    // Not while a frame is in flight: it holds the game.
    afterFrame(() => game?.pause());
  } else if (m.type === "resume") {
    if (!paused) return;
    paused = false;
    afterFrame(() => {
      if (game && !stopped) {
        game.resume();
        loop();
      }
    });
  }
};

// Run `f` when no frame is in flight (in order).
function afterFrame(f) {
  if (inflight) inflight.then(f);
  else f();
}

// The frame loop (started over after a pause: no burst of frames to catch up).
function loop() {
  let next = performance.now();
  let frames = 0;
  let mark = next;
  let seconds = 0;
  // Game frames due by now (at most 2 to catch up; further behind, the game
  // slows down instead of stuttering). A frame is a promising call: it
  // returns to the event loop whenever the guest threads switch, so the
  // next tick waits for it.
  const run = async (now) => {
    let ran = 0;
    while (now >= next && ran < 2) {
      if (!(await promisingFrame(game.__wbg_ptr, buttons))) {
        stopped = true;
        postMessage({ type: "error", message: "the game stopped" });
        return false;
      }
      next += FIELD_MS;
      ran++;
      frames++;
    }
    if (now - next > 100) next = now;
    if (ran > 0) {
      game.present();
      const a = game.audio();
      if (a.length) postMessage({ type: "audio", samples: a }, [a.buffer]);
    }
    return true;
  };
  const tick = (now) => {
    if (paused) return;
    if (inflight) {
      raf = requestAnimationFrame(tick);
      return;
    }
    if (size) {
      game.resize(size[0], size[1]);
      size = null;
    }
    inflight = run(now)
      .then((alive) => {
        if (!alive || paused) return;
        if (now - mark >= 1000) {
          postMessage({ type: "fps", fps: (frames * 1000) / (now - mark) });
          if (++seconds % 10 === 0) console.log(`[ssx3] wasm memory ${mb()} MB`);
          frames = 0;
          mark = now;
        }
        raf = requestAnimationFrame(tick);
      })
      .catch(crash)
      .finally(() => {
        inflight = null;
      });
  };
  raf = requestAnimationFrame(tick);
}
