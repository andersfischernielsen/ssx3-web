//! SSX 3 in the browser. Runs in a Web Worker (`www/worker.js`): the disc
//! image is a `File` the page hands over, read synchronously with
//! `FileReaderSync` (only workers have it); frames are drawn by `ssx3-gfx`
//! with WebGPU into an `OffscreenCanvas` transferred from the page. The EE
//! code is the unwind form (`generated/ee-unwind`): one host thread, so a
//! game frame (`Game::frame`) returns to the browser like any other call.
//!
//! The image: rendered at the canvas's pixel size, the game's 4:3 view
//! boxed in the canvas (`SSX3_ASPECT=fill` fills it), no anisotropic
//! filtering, no interpolation, no texture packs.
//!
//! The game itself is `ssx3-engine` (unwind form): this crate is the
//! browser front end (disc from a `File`, the canvas, the VU helpers).
//!
//! Sound: the mixer's samples go to the page ([`Game::audio`]), which plays
//! them with an AudioWorklet (`www/audio.js`).
//!
//! What the native window has and this has not (yet): the memory card,
//! gamepads and the settings overlay.
#![cfg(target_arch = "wasm32")]

use std::sync::Arc;

use ssx3_engine::ssx3_core::iso::{self, Source};
use ssx3_engine::{vu, Engine, EngineConfig};
use wasm_bindgen::prelude::*;

// Talc instead of the default dlmalloc: allocation and freeing are a few
// percent of a frame, and talc is the faster of the two in wasm.
#[global_allocator]
static TALC: talc::wasm::WasmDynamicTalc = talc::wasm::new_wasm_dynamic_allocator();

/// The settings: what the page passes (`SSX3_*` keys, e.g. from the URL
/// query), else the default, except the ones that would change the
/// original image: no anisotropic filtering, the 4:3 box.
fn settings(page: std::collections::HashMap<String, String>) -> impl ssx3_engine::Settings {
    move |key: &str| match page.get(key) {
        Some(v) => Some(v.clone()),
        None if key == "SSX3_ANISO" => Some("1".into()),
        None if key == "SSX3_ASPECT" => Some("4:3".into()),
        None => None,
    }
}

/// A JS object's string-valued `SSX3_*` properties.
fn page_settings(obj: &JsValue) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    if !obj.is_object() {
        return m;
    }
    for entry in js_sys::Object::entries(obj.unchecked_ref()).iter() {
        let pair: js_sys::Array = entry.unchecked_into();
        if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
            if k.starts_with("SSX3_") {
                m.insert(k, v);
            }
        }
    }
    m
}

fn log(s: &str) {
    web_sys::console::log_1(&s.into());
}

/// The disc image: byte ranges of the `File`, read synchronously.
struct BlobSource {
    blob: web_sys::Blob,
    reader: web_sys::FileReaderSync,
}

// SAFETY: a wasm32 build without atomics has one thread.
unsafe impl Send for BlobSource {}

impl Source for BlobSource {
    fn len(&self) -> u64 {
        self.blob.size() as u64
    }

    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let err = |e: JsValue| std::io::Error::other(format!("{e:?}"));
        let part = self
            .blob
            .slice_with_f64_and_f64(off as f64, (off + buf.len() as u64) as f64)
            .map_err(err)?;
        let ab = self.reader.read_as_array_buffer(&part).map_err(err)?;
        let a = js_sys::Uint8Array::new(&ab);
        if a.length() as usize != buf.len() {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        a.copy_to(buf);
        Ok(())
    }
}

struct SendBlob(web_sys::Blob);
// SAFETY: as for `BlobSource`.
unsafe impl Send for SendBlob {}
unsafe impl Sync for SendBlob {}

/// The VU helper workers' shared buffer (`www/vuworker.js`). Int32 header:
/// [0] generation, [1] job count, [2] next job (generation << 16 | index),
/// [3] jobs done, [4] output base, [5] arena bytes per helper, [6] store
/// generation, [7] store bytes used, [8] store base; from int 16, per job:
/// input offset, length, output offset (-1: did not fit), length. Inputs
/// follow the header, then the store ([`vu::SharedStore`]:
/// replayed calls' input, written once; helpers mirror it), then each
/// helper's output arena.
///
/// The store starts over (a new generation) when it is full, but only
/// between batches: the jobs of a batch hold offsets into it until they
/// have run. Within a batch, input that does not fit is carried by its job.
mod vu_pool {
    use js_sys::{Atomics, Int32Array, SharedArrayBuffer, Uint8Array};
    use ssx3_engine::vu;

    const HEADER: u32 = 65536;
    const TABLE: u32 = 16;
    const MAX_JOBS: usize = ((HEADER / 4 - TABLE) / 4) as usize;
    const INPUTS: u32 = 32 << 20;
    /// The store's size unless `SSX3_VU_STORE_MB` says otherwise (a small
    /// one starts over every few frames: a check of that path).
    pub const STORE: u32 = 24 << 20;
    const ARENA: u32 = 8 << 20;

    pub struct Pool {
        pub sab: SharedArrayBuffer,
        i32: Int32Array,
        u8: Uint8Array,
        /// The store's size (bytes).
        store_size: u32,
        /// The store's contents here too (jobs run on this thread).
        store: std::sync::Mutex<Store>,
    }

    #[derive(Default)]
    struct Store {
        at: std::collections::HashMap<u64, u32, foldhash::fast::FixedState>,
        bytes: Vec<u8>,
        generation: i32,
        /// Input did not fit: start over once the batch has run.
        full: bool,
    }

    // SAFETY: a wasm32 build without atomics has one thread; the helpers
    // only touch the buffer through their own JS views.
    unsafe impl Send for Pool {}
    unsafe impl Sync for Pool {}

    impl Pool {
        pub fn new(helpers: u32, store_size: u32) -> Option<Pool> {
            let sab = SharedArrayBuffer::new(HEADER + INPUTS + store_size + helpers * ARENA);
            let (i32, u8) = (Int32Array::new(&sab), Uint8Array::new(&sab));
            i32.set_index(4, (HEADER + INPUTS + store_size) as i32);
            i32.set_index(5, ARENA as i32);
            i32.set_index(8, (HEADER + INPUTS) as i32);
            Some(Pool {
                sab,
                i32,
                u8,
                store_size,
                store: Default::default(),
            })
        }

        /// [`super::vu::SharedStore`]: data under `key`, appended
        /// once; none when it does not fit (the store starts over after
        /// the batch, [`Pool::restart_if_full`]).
        fn store_offset(&self, key: u64, bytes: &mut dyn FnMut() -> Vec<u8>) -> Option<u32> {
            let mut s = self.store.lock().unwrap();
            if let Some(&o) = s.at.get(&key) {
                return Some(o);
            }
            let b = bytes();
            if b.len() > self.store_size as usize / 4 {
                return None;
            }
            if s.bytes.len() + b.len() > self.store_size as usize {
                s.full = true;
                return None;
            }
            let o = s.bytes.len() as u32;
            self.u8
                .subarray(HEADER + INPUTS + o, HEADER + INPUTS + o + b.len() as u32)
                .copy_from(&b);
            s.bytes.extend_from_slice(&b);
            s.at.insert(key, o);
            self.i32.set_index(7, s.bytes.len() as i32);
            Some(o)
        }

        /// After a batch has run: start the store over if input did not
        /// fit (the helpers mirror the new generation at the next batch).
        fn restart_if_full(&self) {
            let mut s = self.store.lock().unwrap();
            if s.full {
                s.at.clear();
                s.bytes.clear();
                s.full = false;
                s.generation += 1;
                self.i32.set_index(7, 0);
                self.i32.set_index(6, s.generation);
            }
        }

        fn claim(&self, tag: i32, n: i32) -> Option<usize> {
            loop {
                let v = Atomics::load(&self.i32, 2).ok()?;
                if v & !0xffff != tag || (v & 0xffff) >= n {
                    return None;
                }
                if Atomics::compare_exchange(&self.i32, 2, v, v + 1).ok()? == v {
                    return Some((v & 0xffff) as usize);
                }
            }
        }

        /// Start `jobs` on the helpers; the returned function waits for
        /// them (taking any not started yet itself) and gives the results
        /// in order.
        pub fn start(self: &std::sync::Arc<Self>, jobs: Vec<Vec<u8>>) -> vu::Collect {
            let local = |j: &[u8]| vu::run_job(j, &self.store.lock().unwrap().bytes);
            let size: usize = jobs.iter().map(|j| j.len()).sum();
            if jobs.len() > MAX_JOBS || size > INPUTS as usize {
                let r: Vec<Vec<u8>> = jobs.iter().map(|j| local(j)).collect();
                self.restart_if_full();
                return Box::new(move || r);
            }
            let n = jobs.len() as i32;
            let mut off = HEADER;
            for (j, job) in jobs.iter().enumerate() {
                self.u8.subarray(off, off + job.len() as u32).copy_from(job);
                let t = TABLE + 4 * j as u32;
                self.i32.set_index(t, off as i32);
                self.i32.set_index(t + 1, job.len() as i32);
                self.i32.set_index(t + 2, -1);
                off += job.len() as u32;
            }
            let gen = (self.i32.get_index(0) + 1) & 0x7fff;
            let tag = gen << 16;
            let _ = Atomics::store(&self.i32, 1, n);
            let _ = Atomics::store(&self.i32, 3, 0);
            let _ = Atomics::store(&self.i32, 2, tag);
            let _ = Atomics::store(&self.i32, 0, gen);
            let _ = Atomics::notify(&self.i32, 0);
            let pool = self.clone();
            Box::new(move || pool.collect(jobs, tag, n))
        }

        fn collect(&self, jobs: Vec<Vec<u8>>, tag: i32, n: i32) -> Vec<Vec<u8>> {
            let local = |j: &[u8]| vu::run_job(j, &self.store.lock().unwrap().bytes);
            let mut out: Vec<Option<Vec<u8>>> = vec![None; jobs.len()];
            while let Some(j) = self.claim(tag, n) {
                out[j] = Some(local(&jobs[j]));
                let _ = Atomics::add(&self.i32, 3, 1);
            }
            loop {
                let d = Atomics::load(&self.i32, 3).unwrap_or(n);
                if d >= n {
                    break;
                }
                let _ = Atomics::wait(&self.i32, 3, d);
            }
            let results = out
                .into_iter()
                .enumerate()
                .map(|(j, r)| {
                    r.unwrap_or_else(|| {
                        let t = TABLE + 4 * j as u32;
                        let (o, len) = (self.i32.get_index(t + 2), self.i32.get_index(t + 3));
                        if o < 0 {
                            return local(&jobs[j]);
                        }
                        let mut v = vec![0u8; len as usize];
                        self.u8.subarray(o as u32, (o + len) as u32).copy_to(&mut v);
                        v
                    })
                })
                .collect();
            self.restart_if_full();
            results
        }
    }

    /// The pool as the renderer's store of shared input.
    pub struct PoolStore(pub std::sync::Arc<Pool>);

    impl vu::SharedStore for PoolStore {
        fn offset(&mut self, key: u64, bytes: &mut dyn FnMut() -> Vec<u8>) -> Option<u32> {
            self.0.store_offset(key, bytes)
        }
    }
}

/// The largest box of aspect `a` (width / height) in `w` x `h`.
fn fit(w: u32, h: u32, a: f32) -> (f32, f32) {
    let (w, h) = (w as f32, h as f32);
    let (bw, bh) = if w / h > a { (h * a, h) } else { (w, w / a) };
    (bw.floor().max(64.0), bh.floor().max(64.0))
}

/// Draws the renderer's image (an sRGB view) to the canvas.
const BLIT: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
// The present marker (`SSX3_PRESENT_MARKER=1`): x the present count, y the
// game frame, z on. Bit cells in the image's bottom-left corner, lowest bit
// first: the bottom row the present count, the row above the game frame.
@group(0) @binding(2) var<uniform> mark: vec4<u32>;
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}
@fragment fn fs(v: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, smp, v.uv).rgb;
    if (mark.z != 0u && v.uv.x < 0.25 && v.uv.y > 0.94) {
        let bit = u32(v.uv.x * 64.0);
        let n = select(mark.y, mark.x, v.uv.y > 0.97);
        return vec4<f32>(vec3<f32>(f32((n >> bit) & 1u)), 1.0);
    }
    return vec4<f32>(c, 1.0);
}
"#;

#[wasm_bindgen]
pub struct Game {
    engine: Engine,
    gfx: ssx3_engine::Renderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    view_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    /// The present marker's uniform (`SSX3_PRESENT_MARKER`) and the
    /// presents so far.
    mark: wgpu::Buffer,
    mark_on: bool,
    presents: u32,
    started: f64,
    /// ms in `run_frame` (game and, in it, the renderer) and in `present`
    /// since the last report.
    ms_frame: f64,
    ms_present: f64,
    /// The VU helper workers' buffer (none without cross-origin isolation).
    vu_buffer: Option<js_sys::SharedArrayBuffer>,
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// Boot the game: `canvas` is drawn to, `iso` is the disc image, `helpers`
/// the number of VU helper workers the page will start (0: none; needs
/// cross-origin isolation for the shared buffer), `settings` an object of
/// `SSX3_*` settings (`ssx3/docs/options.md`; strings) or `undefined`.
#[wasm_bindgen]
pub async fn start(
    canvas: web_sys::OffscreenCanvas,
    iso: web_sys::Blob,
    helpers: u32,
    settings: JsValue,
) -> Result<Game, JsValue> {
    let page = page_settings(&settings);
    let mark_on = page.get("SSX3_PRESENT_MARKER").is_some_and(|v| v != "0");
    let store_size = page
        .get("SSX3_VU_STORE_MB")
        .and_then(|v| v.parse::<u32>().ok())
        .map_or(vu_pool::STORE, |mb| mb.clamp(1, 256) << 20);
    console_error_panic_hook::set_once();
    let blob = Arc::new(SendBlob(iso));
    iso::set_opener(Box::new(move || {
        let reader = web_sys::FileReaderSync::new()
            .map_err(|e| std::io::Error::other(format!("FileReaderSync: {e:?}")))?;
        Ok(Box::new(BlobSource {
            blob: blob.0.clone(),
            reader,
        }) as Box<dyn Source>)
    }));
    let ctx = ssx3_engine::ssx3_ps2::gpu::init_context()
        .await
        .ok_or("WebGPU is not available")?;
    let (w, h) = (canvas.width().max(64), canvas.height().max(64));
    let surface = ctx
        .instance
        .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas))
        .map_err(|e| format!("surface: {e}"))?;
    let caps = surface.get_capabilities(&ctx.adapter);
    let format = *caps.formats.first().ok_or("no surface format")?;
    let view_format = format.add_srgb_suffix();
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: w,
        height: h,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![view_format],
        color_space: Default::default(),
    };
    surface.configure(&ctx.device, &config);
    let shader = ctx
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT.into()),
        });
    let pipeline = ctx
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(view_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
    let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    log("[ssx3] booting");
    if !page.is_empty() {
        log(&format!("[ssx3] settings: {page:?}"));
    }
    let mut engine = Engine::boot(EngineConfig {
        // The page plays the sound (`Game::audio`).
        audio: ssx3_engine::Audio::Pull,
        // No files in the browser: don't keep pipelines between runs.
        save_pipelines: false,
        ..EngineConfig::new(Box::new(self::settings(page)))
    });
    let gfx = engine.renderer().cloned().ok_or("renderer: no GPU")?;
    let pool = (helpers > 0)
        .then(|| vu_pool::Pool::new(helpers, store_size))
        .flatten();
    let vu_buffer = pool.as_ref().map(|p| p.sab.clone());
    if let Some(p) = pool {
        log(&format!("[ssx3] {helpers} VU helper workers"));
        let p = std::sync::Arc::new(p);
        vu::set_store(Box::new(vu_pool::PoolStore(p.clone())));
        vu::set_offload(Box::new(move |jobs| p.start(jobs)), helpers as usize);
    }
    engine.set_surface_size(w, h);
    Ok(Game {
        engine,
        gfx,
        device: ctx.device.clone(),
        queue: ctx.queue.clone(),
        surface,
        config,
        view_format,
        pipeline,
        sampler,
        started: now_ms(),
        ms_frame: 0.0,
        ms_present: 0.0,
        mark: ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("present marker"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        mark_on,
        presents: 0,
        vu_buffer,
    })
}

#[wasm_bindgen]
impl Game {
    /// The buffer to hand to the VU helper workers.
    pub fn vu_buffer(&self) -> Option<js_sys::SharedArrayBuffer> {
        self.vu_buffer.clone()
    }

    /// The canvas was resized (device pixels): the engine renders at the
    /// size of the box the image fills in it (`present`).
    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (w, h) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        self.engine.set_surface_size(w, h);
    }

    /// Run one game frame with pad 1's buttons (libpad bits, pressed = 1).
    /// False when the game has stopped.
    pub fn frame(&mut self, buttons: u16) -> bool {
        self.engine.set_pad(0, buttons);
        let t0 = now_ms();
        let alive = self.engine.step();
        self.ms_frame += now_ms() - t0;
        if self.engine.frames() % 300 == 0 {
            log(&format!(
                "[{:6.1}s] {}",
                (now_ms() - self.started) / 1e3,
                self.engine.status()
            ));
            log(&format!(
                "  last 300 frames: {:.2} ms per frame in run_frame, {:.2} ms in present; {}",
                self.ms_frame / 300.0,
                self.ms_present / 300.0,
                self.engine.timing_report()
            ));
            self.ms_frame = 0.0;
            self.ms_present = 0.0;
        }
        if !alive {
            log(&format!("halted: {:?}", self.engine.halted()));
        }
        alive
    }

    /// The page is hidden (`www/worker.js` on the page's `visibilitychange`):
    /// silence the sound until [`Game::resume`]; the worker's loop stops
    /// running frames meanwhile.
    pub fn pause(&mut self) {
        self.engine.pause();
    }

    pub fn resume(&mut self) {
        self.engine.resume();
    }

    /// The sound since the last call: interleaved stereo, 48 kHz.
    pub fn audio(&mut self) -> Vec<i16> {
        let mut v = Vec::new();
        self.engine.audio_pull(&mut v);
        v
    }

    /// Draw the latest image to the canvas.
    pub fn present(&mut self) {
        let t0 = now_ms();
        self.present_image();
        self.ms_present += now_ms() - t0;
    }

    fn present_image(&mut self) {
        let Some((view, iw, ih)) = self.gfx.present_view() else {
            return;
        };
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let target = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.mark.as_entire_binding(),
                },
            ],
        });
        self.presents = self.presents.wrapping_add(1);
        let mark = [
            self.presents,
            self.engine.frames() as u32,
            self.mark_on as u32,
            0,
        ];
        let bytes: Vec<u8> = mark.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.queue.write_buffer(&self.mark, 0, &bytes);
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // The image at its own aspect, as large as fits, centred.
            let (cw, ch) = (self.config.width as f32, self.config.height as f32);
            let (w, h) = fit(self.config.width, self.config.height, iw as f32 / ih as f32);
            pass.set_viewport(
                ((cw - w) / 2.0).floor(),
                ((ch - h) / 2.0).floor(),
                w,
                h,
                0.0,
                1.0,
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(enc.finish()));
        self.queue.present(frame);
    }
}
