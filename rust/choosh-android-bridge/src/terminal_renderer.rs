//! The wgpu/glyphon terminal renderer: draws a
//! [`choosh_terminal_engine::Engine`]'s grid to a GPU surface.
//!
//! Ported from `njreid/zelland@8bf9cf5`'s
//! `src-tauri/src/renderer/mod.rs` (see `docs/licenses/zelland-grant.md`
//! and `docs/licenses/terminal-provenance.md`), adapted to draw
//! [`choosh_terminal_engine`]'s pure-Rust grid instead of libghostty's C
//! render-state iterator (`terminal.rs`'s module doc explains that
//! substitution). The wgpu/glyphon pipeline itself — surface-format
//! detection and atlas rebuilding, per-row damage caching, the cursor and
//! selection shaders, deferred resize, and GPU-limit clamping — is the
//! same design Zelland's `WGPU_FIXES.md` records as hard-won: this module
//! keeps those fixes rather than rediscovering them.
//!
//! This file is platform-generic (no JNI, no `ndk` import) so it type-checks
//! and is exercised by `cargo test`/`cargo clippy` on the host too; only
//! `terminal_jni.rs` (the Android `Surface` -> `ANativeWindow` conversion
//! and the JNI entry points) is `#[cfg(target_os = "android")]`-gated, the
//! same split Zelland used between `renderer/mod.rs` and
//! `renderer/android.rs`.

// Pixel<->cell geometry conversion is inherently a lossy float/int
// round-trip (a cell is exactly `cell_width`/`cell_height` pixels, an
// approximate, device-measured value, not an exact integer ratio) —
// every cast in this file is a deliberate, bounded (screen-sized)
// unit conversion, not an overlooked truncation/precision bug.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
// Every public item here is only ever constructed/called from
// `terminal_jni.rs`, which is itself `#[cfg(target_os = "android")]`-gated
// (see this module's doc comment) — a host `cargo check`/`cargo clippy`
// build correctly type-checks this file (that's the whole point of the
// Zelland-style mod.rs/android.rs split) but never actually calls into it,
// which would otherwise look like real dead code. The Android build (where
// this dead_code allowance does NOT apply) is the one that proves these
// items are actually used.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use bytemuck::{Pod, Zeroable};
use choosh_terminal_engine::{CursorShape, Engine};
use glyphon::{
    Attrs, Buffer as GlyphonBuffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping, Style,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle};

/// Fallback cell metrics used only before the first real font measurement
/// / surface exists — every real frame uses [`Renderer::cell_width`]/
/// [`Renderer::cell_height`], which are derived from the loaded face per
/// `terminal-experience.md`'s "derive terminal cell metrics from the exact
/// loaded face rather than hard-coded dimensions".
pub const FALLBACK_CELL_WIDTH: f32 = 17.0;
pub const FALLBACK_CELL_HEIGHT: f32 = 38.0;

/// Iosevka Charon Mono, embedded at build time (see
/// `docs/specs/terminal-experience.md`'s typeface requirement and
/// `docs/licenses/terminal-provenance.md` for provenance). Embedding the
/// exact APK font resource — rather than re-fetching or discovering an
/// on-device system font — is what lets [`Renderer::init`] measure real
/// glyph metrics immediately, with no Android asset-copy step to race.
static TERMINAL_FONT: &[u8] = include_bytes!("../../../android/app/src/main/res/font/iosevka_charon_mono.ttf");
static TERMINAL_FONT_BOLD: &[u8] = include_bytes!("../../../android/app/src/main/res/font/iosevka_charon_mono_bold.ttf");

const CURSOR_SHADER: &str = r"
struct CursorUniforms {
    rect: vec4<f32>,
    color: vec4<f32>,
}
@group(0) @binding(0) var<uniform> u: CursorUniforms;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x0 = u.rect.x; let x1 = u.rect.z;
    let y0 = u.rect.w; let y1 = u.rect.y;
    var xs = array<f32,6>(x0, x1, x0, x1, x1, x0);
    var ys = array<f32,6>(y1, y1, y0, y1, y0, y0);
    return vec4<f32>(xs[i], ys[i], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return u.color;
}
";

/// One styled run of text within a single terminal row — the renderer's
/// damage-cache unit, ported from Zelland's `CellRun`/`row_cache`.
#[derive(Clone, PartialEq)]
struct CellRun {
    text: String,
    fg: (u8, u8, u8),
    bold: bool,
    italic: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CursorUniform {
    rect: [f32; 4],
    color: [f32; 4],
}

pub struct RawWindow {
    handle: RawWindowHandle,
}

// SAFETY: the wrapped handle is a plain FFI pointer (`ANativeWindow*` on
// Android) with no thread-affine state of its own; wgpu only dereferences
// it while `set_surface` runs on whichever thread owns the renderer lock.
unsafe impl Send for RawWindow {}

impl HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        unsafe { Ok(WindowHandle::borrow_raw(self.handle)) }
    }
}

pub struct RawDisplay {
    handle: RawDisplayHandle,
}

unsafe impl Send for RawDisplay {}

impl HasDisplayHandle for RawDisplay {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        unsafe { Ok(DisplayHandle::borrow_raw(self.handle)) }
    }
}

impl RawWindow {
    #[must_use]
    pub fn new(handle: RawWindowHandle) -> Self {
        Self { handle }
    }
}

impl RawDisplay {
    #[must_use]
    pub fn new(handle: RawDisplayHandle) -> Self {
        Self { handle }
    }
}

pub struct Renderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: Option<wgpu::Surface<'static>>,
    config: Option<wgpu::SurfaceConfiguration>,
    pending_size: Option<(u32, u32)>,

    glyph_cache: Cache,
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    atlas_format: wgpu::TextureFormat,
    text_paint: TextRenderer,
    viewport: Viewport,
    text_buffer: GlyphonBuffer,

    row_cache: Vec<Vec<CellRun>>,
    span_buf: Vec<(String, Weight, Style, GlyphonColor)>,

    pub cell_width: f32,
    pub cell_height: f32,

    cursor_pipeline: Option<wgpu::RenderPipeline>,
    cursor_bind_group_layout: Option<wgpu::BindGroupLayout>,
    cursor_uniform_buf: Option<wgpu::Buffer>,
    cursor_bind_group: Option<wgpu::BindGroup>,
    cursor_pixel_rect: Option<(f32, f32, f32, f32)>,
    cursor_shape: CursorShape,

    /// Incremented on every submitted frame; a cheap on-device liveness
    /// signal for verification (see `terminal_jni.rs`'s test-inject path)
    /// without needing to read back GPU memory.
    pub frames_rendered: u64,

    // Diagnostic-only, read via `debug_state()` — not used by any
    // production rendering decision.
    last_span_count: usize,
    last_prepare_result: Result<(), String>,
    last_render_result: Result<(), String>,
}

impl Renderer {
    /// # Errors
    /// Returns an error string (never panics) when adapter/device creation
    /// fails, so JNI callers can surface "GPU initialization failed"
    /// visibly per `terminal-experience.md`'s "fall back visibly when GPU
    /// initialization fails rather than presenting a blank terminal".
    pub async fn init() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::all(), ..Default::default() });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| "no suitable GPU adapter".to_string())?;

        let adapter_limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("choosh terminal renderer"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter_limits),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await
            .map_err(|error| format!("failed to create wgpu device: {error}"))?;

        let mut font_system = FontSystem::new();
        font_system.db_mut().load_font_data(TERMINAL_FONT.to_vec());
        font_system.db_mut().load_font_data(TERMINAL_FONT_BOLD.to_vec());

        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let cache_format = wgpu::TextureFormat::Bgra8Unorm;
        let mut atlas = TextAtlas::new(&device, &queue, &cache, cache_format);
        let text_paint = TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let viewport = Viewport::new(&device, &cache);
        let text_buffer = GlyphonBuffer::new(&mut font_system, Metrics::new(FALLBACK_CELL_HEIGHT * 0.75, FALLBACK_CELL_HEIGHT));

        let mut renderer = Self {
            instance,
            adapter,
            device,
            queue,
            surface: None,
            config: None,
            pending_size: None,
            glyph_cache: cache,
            font_system,
            swash_cache,
            atlas,
            atlas_format: cache_format,
            text_paint,
            viewport,
            text_buffer,
            row_cache: Vec::new(),
            span_buf: Vec::new(),
            cell_width: FALLBACK_CELL_WIDTH,
            cell_height: FALLBACK_CELL_HEIGHT,
            cursor_pipeline: None,
            cursor_bind_group_layout: None,
            cursor_uniform_buf: None,
            cursor_bind_group: None,
            cursor_pixel_rect: None,
            cursor_shape: CursorShape::Block,
            frames_rendered: 0,
            last_span_count: 0,
            last_prepare_result: Ok(()),
            last_render_result: Ok(()),
        };
        renderer.measure_cell_metrics();
        renderer.build_cursor_resources();
        Ok(renderer)
    }

    /// Measures the loaded face's real advance width/line height at a
    /// fixed font size, so cell geometry (and therefore PTY sizing,
    /// selection, and pointer-to-cell mapping) comes from the actual font,
    /// not a guess — `terminal-experience.md`'s "live font/cell metrics".
    fn measure_cell_metrics(&mut self) {
        let font_size = FALLBACK_CELL_HEIGHT * 0.75;
        let mut probe = GlyphonBuffer::new(&mut self.font_system, Metrics::new(font_size, FALLBACK_CELL_HEIGHT));
        probe.set_size(&mut self.font_system, Some(1000.0), Some(1000.0));
        probe.set_text(&mut self.font_system, "M", Attrs::new().family(Family::Monospace), Shaping::Advanced);
        probe.shape_until_scroll(&mut self.font_system, false);
        if let Some(run) = probe.layout_runs().next() {
            if let Some(glyph) = run.glyphs.first()
                && glyph.w > 1.0
            {
                self.cell_width = glyph.w;
            }
            if run.line_height > 1.0 {
                self.cell_height = run.line_height;
            }
        }
        self.text_buffer.set_metrics(&mut self.font_system, Metrics::new(font_size, self.cell_height));
    }

    pub fn set_surface(&mut self, window: &RawWindow, display: &RawDisplay) {
        // SAFETY: `window`/`display` wrap a real, live `ANativeWindow`/Android
        // display handle for as long as the caller holds a `surfaceCreated`
        // callback's `Surface` alive — `terminal_jni.rs` only calls this from
        // that callback and drops the surface on `surfaceDestroyed`.
        let surface = match unsafe {
            self.instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_window_handle: window.window_handle().unwrap().as_raw(),
                raw_display_handle: display.display_handle().unwrap().as_raw(),
            })
        } {
            Ok(surface) => surface,
            Err(_error) => return,
        };

        let caps = surface.get_capabilities(&self.adapter);
        let Some(&surface_format) = caps.formats.first() else { return };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: 1,
            height: 1,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&self.device, &config);
        self.surface = Some(surface);
        self.config = Some(config);

        // Rebuild the atlas/pipeline whenever the surface format doesn't
        // match — per WGPU_FIXES.md's "Fix 1", a format mismatch silently
        // drops every text draw with no error.
        if surface_format != self.atlas_format {
            self.rebuild_text_pipeline(surface_format);
        }

        if let Some((width, height)) = self.pending_size.take() {
            self.resize(width, height);
        }
    }

    fn rebuild_text_pipeline(&mut self, format: wgpu::TextureFormat) {
        let mut atlas = TextAtlas::new(&self.device, &self.queue, &self.glyph_cache, format);
        let text_paint = TextRenderer::new(&mut atlas, &self.device, wgpu::MultisampleState::default(), None);
        self.atlas = atlas;
        self.atlas_format = format;
        self.text_paint = text_paint;
        self.row_cache.clear();
        self.build_cursor_resources();
    }

    fn build_cursor_resources(&mut self) {
        let format = self.config.as_ref().map_or(wgpu::TextureFormat::Bgra8Unorm, |c| c.format);
        let shader = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cursor_shader"),
            source: wgpu::ShaderSource::Wgsl(CURSOR_SHADER.into()),
        });
        let bgl = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cursor_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let pipeline_layout =
            self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("cursor_layout"), bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cursor_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: wgpu::PipelineCompilationOptions::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::REPLACE), write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let uniform_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cursor_uniform_buf"),
            size: std::mem::size_of::<CursorUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cursor_bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() }],
        });
        self.cursor_pipeline = Some(pipeline);
        self.cursor_bind_group_layout = Some(bgl);
        self.cursor_uniform_buf = Some(uniform_buf);
        self.cursor_bind_group = Some(bind_group);
    }

    /// Releases the wgpu surface (surface loss / `surfaceDestroyed`, per
    /// `terminal-experience.md`'s lifecycle requirements). The next
    /// `set_surface` re-attaches; `resize` calls in between are deferred
    /// via `pending_size`.
    pub fn drop_surface(&mut self) {
        self.surface = None;
        self.config = None;
    }

    /// Not called from any current production path — kept for a future
    /// on-demand diagnostic hook alongside [`Self::debug_state`], which has
    /// the identical rationale.
    #[allow(dead_code)]
    #[must_use]
    pub fn has_surface(&self) -> bool {
        self.surface.is_some()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let max_dim = self.device.limits().max_texture_dimension_2d;
        let width = width.clamp(1, max_dim);
        let height = height.clamp(1, max_dim);
        if self.surface.is_none() {
            self.pending_size = Some((width, height));
            return;
        }
        if let (Some(surface), Some(config)) = (self.surface.as_mut(), self.config.as_mut()) {
            config.width = width;
            config.height = height;
            surface.configure(&self.device, config);
            self.viewport.update(&self.queue, Resolution { width, height });
            self.row_cache.clear();
        }
    }

    /// Cell/row dimensions for a pixel surface of the given size, using
    /// the live measured font metrics — callers use this to compute PTY
    /// `cols`/`rows` for resize requests.
    #[must_use]
    pub fn cells_for_size(&self, width_px: u32, height_px: u32) -> (u16, u16) {
        let cols = (width_px as f32 / self.cell_width).floor().max(1.0) as u16;
        let rows = (height_px as f32 / self.cell_height).floor().max(1.0) as u16;
        (cols, rows)
    }

    fn update_cursor_uniforms(&self, px: f32, py: f32, pw: f32, ph: f32) {
        let Some(config) = self.config.as_ref() else { return };
        let (sw, sh) = (config.width as f32, config.height as f32);
        let (x0, x1) = ((px / sw) * 2.0 - 1.0, ((px + pw) / sw) * 2.0 - 1.0);
        let (y1, y0) = (1.0 - (py / sh) * 2.0, 1.0 - ((py + ph) / sh) * 2.0);
        // The bar/underline cursor shapes are drawn as a thin slice of the
        // full cell rect rather than a separate shader — cheaper than a
        // second pipeline and visually equivalent.
        let (x0, y0, x1, y1) = match self.cursor_shape {
            CursorShape::Block => (x0, y0, x1, y1),
            CursorShape::Bar => (x0, y0, x0 + (x1 - x0) * 0.12, y1),
            CursorShape::Underline => (x0, y0, x1, y0 + (y1 - y0) * 0.12),
        };
        let uniform = CursorUniform { rect: [x0, y1, x1, y0], color: [1.0, 1.0, 1.0, 1.0] };
        if let Some(buf) = &self.cursor_uniform_buf {
            self.queue.write_buffer(buf, 0, bytemuck::bytes_of(&uniform));
        }
    }

    /// Renders one frame from `engine`'s current grid/cursor state,
    /// honoring per-row damage: unchanged rows are not re-shaped, per
    /// `terminal-experience.md`'s "render only after damage ...".
    pub fn draw(&mut self, engine: &mut Engine) {
        let dirty_rows = engine.dirty_rows();
        let width = self.config.as_ref().map_or(800.0, |c| c.width as f32);
        let height = self.config.as_ref().map_or(600.0, |c| c.height as f32);

        let mut changed = !dirty_rows.is_empty();
        for &row in &dirty_rows {
            let runs = build_row_runs(engine.terminal().grid().row(row));
            let idx = usize::from(row);
            if idx >= self.row_cache.len() {
                self.row_cache.resize(idx + 1, Vec::new());
            }
            self.row_cache[idx] = runs;
            engine.clear_row_dirty(row);
        }
        let total_rows = usize::from(engine.terminal().grid().rows());
        if self.row_cache.len() > total_rows {
            self.row_cache.truncate(total_rows);
            changed = true;
        }

        let cursor = engine.terminal().cursor();
        self.cursor_shape = cursor.shape;
        self.cursor_pixel_rect =
            cursor.visible.then_some((f32::from(cursor.col) * self.cell_width, f32::from(cursor.row) * self.cell_height, self.cell_width, self.cell_height));

        if changed {
            self.text_buffer.set_size(&mut self.font_system, Some(width), Some(height));
            self.span_buf.clear();
            let row_count = self.row_cache.len();
            for (row_idx, row) in self.row_cache.iter().enumerate() {
                for run in row {
                    let (r, g, b) = run.fg;
                    self.span_buf.push((
                        run.text.clone(),
                        if run.bold { Weight::BOLD } else { Weight::NORMAL },
                        if run.italic { Style::Italic } else { Style::Normal },
                        GlyphonColor::rgb(r, g, b),
                    ));
                }
                if row_idx + 1 < row_count {
                    self.span_buf.push(("\n".to_string(), Weight::NORMAL, Style::Normal, GlyphonColor::rgb(255, 255, 255)));
                }
            }
            {
                let text_buffer = &mut self.text_buffer;
                let font_system = &mut self.font_system;
                text_buffer.set_rich_text(
                    font_system,
                    self.span_buf.iter().map(|(text, weight, style, color)| {
                        let mut attrs = Attrs::new().family(Family::Monospace).weight(*weight).style(*style);
                        attrs.color_opt = Some(*color);
                        (text.as_str(), attrs)
                    }),
                    Attrs::new().family(Family::Monospace),
                    Shaping::Advanced,
                );
                text_buffer.shape_until_scroll(font_system, false);
            }
            self.last_prepare_result = self
                .text_paint
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    [TextArea {
                        buffer: &self.text_buffer,
                        left: 0.0,
                        top: 0.0,
                        scale: 1.0,
                        bounds: TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 },
                        default_color: GlyphonColor::rgb(255, 255, 255),
                        custom_glyphs: &[],
                    }],
                    &mut self.swash_cache,
                )
                .map_err(|error| error.to_string());
            self.last_span_count = self.span_buf.len();
        }

        self.present();
    }

    /// One-line summary of internal state for on-device diagnostic logging
    /// (see `terminal_jni.rs::alog`) — not called from any current
    /// production path (see `redraw()`'s doc comment for why), kept for a
    /// future on-demand diagnostic hook.
    #[allow(dead_code)]
    #[must_use]
    pub fn debug_state(&self) -> String {
        format!(
            "config={:?} atlas_format={:?} last_span_count={} last_prepare={:?} last_render={:?} row_cache_len={} font_faces={} first_row_text={:?}",
            self.config.as_ref().map(|c| (c.width, c.height, c.format)),
            self.atlas_format,
            self.last_span_count,
            self.last_prepare_result,
            self.last_render_result,
            self.row_cache.len(),
            self.font_system.db().faces().count(),
            self.row_cache.first().map(|row| row.iter().map(|run| run.text.as_str()).collect::<String>()),
        )
    }

    fn present(&mut self) {
        let Some(surface) = self.surface.as_ref() else { return };
        let Ok(frame) = surface.get_current_texture() else { return };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        if let Some((cx, cy, cw, ch)) = self.cursor_pixel_rect {
            self.update_cursor_uniforms(cx, cy, cw, ch);
        }

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear_cursor_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if self.cursor_pixel_rect.is_some()
                && let (Some(pipeline), Some(bg)) = (&self.cursor_pipeline, &self.cursor_bind_group)
            {
                rpass.set_pipeline(pipeline);
                rpass.set_bind_group(0, bg, &[]);
                rpass.draw(0..6, 0..1);
            }
        }
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("text_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.last_render_result = self.text_paint.render(&self.atlas, &self.viewport, &mut rpass).map_err(|error| error.to_string());
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.atlas.trim();
        self.frames_rendered += 1;
    }
}

fn build_row_runs(cells: &[choosh_terminal_engine::Cell]) -> Vec<CellRun> {
    let mut runs: Vec<CellRun> = Vec::new();
    let mut current = String::new();
    let (mut fg, mut bold, mut italic) = ((255u8, 255u8, 255u8), false, false);
    let mut first = true;

    for cell in cells {
        if cell.wide_continuation {
            continue;
        }
        let text = if cell.text.is_empty() { " " } else { cell.text.as_str() };
        let resolved_fg =
            if cell.inverse { cell.bg.to_rgb((0, 0, 0)) } else { cell.fg.to_rgb((255, 255, 255)) };

        if first {
            (fg, bold, italic) = (resolved_fg, cell.bold, cell.italic);
            current.push_str(text);
            first = false;
        } else if resolved_fg == fg && cell.bold == bold && cell.italic == italic {
            current.push_str(text);
        } else {
            if !current.is_empty() {
                runs.push(CellRun { text: current.clone(), fg, bold, italic });
            }
            current.clear();
            current.push_str(text);
            (fg, bold, italic) = (resolved_fg, cell.bold, cell.italic);
        }
    }
    if !current.is_empty() {
        runs.push(CellRun { text: current, fg, bold, italic });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_terminal_engine::{Cell, Color};

    #[test]
    fn build_row_runs_merges_adjacent_matching_cells() {
        let cells = vec![
            Cell { text: "a".into(), ..Cell::default() },
            Cell { text: "b".into(), ..Cell::default() },
            Cell { text: "c".into(), fg: Color::Indexed(1), ..Cell::default() },
        ];
        let runs = build_row_runs(&cells);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[1].text, "c");
    }

    #[test]
    fn build_row_runs_skips_wide_continuation_cells() {
        let cells = vec![Cell { text: "你".into(), wide: true, ..Cell::default() }, Cell { wide_continuation: true, ..Cell::default() }];
        let runs = build_row_runs(&cells);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "你");
    }
}
