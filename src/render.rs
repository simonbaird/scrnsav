//! Fullscreen shader renderer built on winit + wgpu.
//!
//! One fullscreen window per active monitor, each drawing a WGSL fragment
//! shader (Shadertoy-style) over a single full-screen triangle. All windows
//! share one GPU device/queue; each has its own surface, pipeline, and
//! uniforms (so per-monitor resolution and format are respected). How it
//! exits depends on [`DismissMode`]: Escape-only for an explicit run, or any
//! input when launched by the idle daemon. Exiting closes every window.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

/// How the saver can be dismissed.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum DismissMode {
    /// Explicit `show`: only the Escape key exits. Stray mouse bumps are ignored.
    #[default]
    EscapeOnly,
    /// Idle-triggered: any key, mouse button, or real movement exits — so the
    /// returning user dismisses it however they touch the machine.
    AnyInput,
}

/// Grace period after launch during which input is ignored, so the keypress or
/// mouse jiggle that triggered the saver doesn't dismiss it instantly.
const GRACE: Duration = Duration::from_millis(700);

/// Effects compiled into the binary, selectable by bare name via `--shader`.
/// The first entry is the default used when no `--shader` is given.
const BUNDLED: &[(&str, &str)] = &[
    ("lines", include_str!("../shaders/lines.wgsl")),
    ("balls", include_str!("../shaders/balls.wgsl")),
    ("plasma", include_str!("../shaders/plasma.wgsl")),
    ("starfield", include_str!("../shaders/starfield.wgsl")),
];

/// Names of the bundled effects, in order (first is the default).
pub fn bundled_names() -> impl Iterator<Item = &'static str> {
    BUNDLED.iter().map(|(name, _)| *name)
}

/// Resolve the `--shader` argument to WGSL source. `None` → the default
/// (first bundled) effect; `random` → a random bundled effect; a bundled name
/// → that bundled source; anything else → read as a file path on disk.
fn resolve_shader(shader: Option<&str>) -> anyhow::Result<String> {
    match shader {
        None => Ok(BUNDLED[0].1.to_string()),
        Some("random") => {
            let index = (random_bits() % BUNDLED.len() as u64) as usize;
            let (name, src) = BUNDLED[index];
            log::info!("random shader: {name}");
            Ok(src.to_string())
        }
        Some(name) => match BUNDLED.iter().find(|(n, _)| *n == name) {
            Some((_, src)) => Ok((*src).to_string()),
            None => std::fs::read_to_string(name).with_context(|| {
                format!("reading shader '{name}' (not a bundled name or readable file)")
            }),
        },
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    time: f32,
    seed: f32,
    resolution: [f32; 2],
}

/// GPU resources shared across every monitor's window.
struct Gpu {
    #[allow(dead_code)] // kept alive for the lifetime of the surfaces
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    shader: wgpu::ShaderModule,
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
}

/// Per-monitor render target.
struct Surf {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    seed: f32,
}

impl Surf {
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(device, &self.config);
    }

    fn render(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, time: f32) {
        let uniforms = Uniforms {
            time,
            seed: self.seed,
            resolution: [self.config.width as f32, self.config.height as f32],
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(device, &self.config);
                return;
            }
            _ => {
                log::warn!("surface frame unavailable this tick");
                return;
            }
        };

        let view = frame.texture.create_view(&Default::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("saver pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        queue.submit(Some(encoder.finish()));
        queue.present(frame);
    }
}

fn make_window(
    event_loop: &ActiveEventLoop,
    monitor: Option<MonitorHandle>,
) -> anyhow::Result<Arc<Window>> {
    let attrs = Window::default_attributes()
        .with_title("scrnsav")
        .with_fullscreen(Some(Fullscreen::Borderless(monitor)));
    let window = Arc::new(event_loop.create_window(attrs).context("creating window")?);
    window.set_cursor_visible(false);
    Ok(window)
}

/// A stable per-monitor phase offset, so each screen renders a distinct
/// variation of the effect. Keyed on the monitor's name when available (stable
/// across restarts), falling back to its index.
fn monitor_seed(monitor: &MonitorHandle, index: usize) -> f32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match monitor.name() {
        Some(name) => name.hash(&mut hasher),
        None => index.hash(&mut hasher),
    }
    // Map the hash into a few full phase turns.
    (hasher.finish() % 100_000) as f32 / 100_000.0 * std::f32::consts::TAU * 8.0
}

/// Bind group layout for the single `Uniforms` buffer, shared by every
/// pipeline (windowed and headless).
fn uniforms_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("uniforms"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// Pipeline layout wrapping the uniforms bind group layout.
fn uniforms_pipeline_layout(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    })
}

/// Build the fullscreen-triangle render pipeline for a given target `format`.
/// Shared by the windowed surfaces and the headless screenshot renderer.
fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("saver pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Pseudo-random bits from `RandomState`'s per-call random keys, for effect
/// selection and phase offsets (not cryptographic use).
fn random_bits() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// A pseudo-random phase offset for one-off shots, in the same range as
/// [`monitor_seed`].
fn random_seed() -> f32 {
    (random_bits() % 100_000) as f32 / 100_000.0 * std::f32::consts::TAU * 8.0
}

/// Build the per-monitor surface, choosing its own preferred format so each
/// output is configured correctly even if they differ.
fn build_surf(gpu: &Gpu, window: Arc<Window>, surface: wgpu::Surface<'static>, seed: f32) -> Surf {
    let size = window.inner_size();
    let caps = surface.get_capabilities(&gpu.adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .unwrap_or(caps.formats[0]);

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::default(),
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&gpu.device, &config);

    let uniform_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("uniform buffer"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("uniforms"),
        layout: &gpu.bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }],
    });

    let pipeline = make_pipeline(&gpu.device, &gpu.pipeline_layout, &gpu.shader, format);

    Surf {
        window,
        surface,
        config,
        pipeline,
        uniform_buf,
        bind_group,
        seed,
    }
}

/// Create the GPU context and one window+surface per active monitor.
fn init(
    event_loop: &ActiveEventLoop,
    shader_src: &str,
) -> anyhow::Result<(Gpu, HashMap<WindowId, Surf>)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    // One fullscreen window per monitor (fall back to a single default window).
    let monitors: Vec<MonitorHandle> = event_loop.available_monitors().collect();
    let mut pairs: Vec<(Arc<Window>, wgpu::Surface<'static>, f32)> = Vec::new();
    if monitors.is_empty() {
        let window = make_window(event_loop, None)?;
        let surface = instance
            .create_surface(window.clone())
            .context("creating wgpu surface")?;
        pairs.push((window, surface, 0.0));
    } else {
        log::info!("spanning {} monitor(s)", monitors.len());
        for (i, monitor) in monitors.into_iter().enumerate() {
            let seed = monitor_seed(&monitor, i);
            let window = make_window(event_loop, Some(monitor))?;
            let surface = instance
                .create_surface(window.clone())
                .context("creating wgpu surface")?;
            pairs.push((window, surface, seed));
        }
    }

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&pairs[0].1),
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .context("no suitable GPU adapter")?;

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("scrnsav device"),
        required_features: wgpu::Features::empty(),
        // Use what the adapter actually supports; downlevel_defaults caps
        // textures at 2048px, too small for a 2560x1440+ display.
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .context("requesting GPU device")?;

    // Capture shader-compile and pipeline-creation validation errors (e.g. a
    // broken `--shader` file, or one that parses but exceeds this GPU's limits)
    // instead of letting wgpu's default handler panic. Scope spans every
    // pipeline too, so it must be popped after the surfaces are built below.
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("saver shader"),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });

    let bind_group_layout = uniforms_bind_group_layout(&device);
    let pipeline_layout = uniforms_pipeline_layout(&device, &bind_group_layout);

    let gpu = Gpu {
        instance,
        adapter,
        device,
        queue,
        shader,
        bind_group_layout,
        pipeline_layout,
    };

    let mut surfaces = HashMap::new();
    for (window, surface, seed) in pairs {
        let surf = build_surf(&gpu, window, surface, seed);
        surfaces.insert(surf.window.id(), surf);
    }

    if let Some(err) = pollster::block_on(error_scope.pop()) {
        anyhow::bail!("shader failed to compile:\n{err}");
    }

    Ok((gpu, surfaces))
}

#[derive(Default)]
struct App {
    gpu: Option<Gpu>,
    surfaces: HashMap<WindowId, Surf>,
    start: Option<Instant>,
    launched_at: Option<Instant>,
    shader_src: String,
    dismiss: DismissMode,
    /// Set if setup failed inside the event loop, so `run` can report it.
    init_error: Option<anyhow::Error>,
}

impl App {
    fn past_grace(&self) -> bool {
        self.launched_at
            .map(|t| t.elapsed() > GRACE)
            .unwrap_or(false)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        match init(event_loop, &self.shader_src) {
            Ok((gpu, surfaces)) => {
                self.gpu = Some(gpu);
                self.surfaces = surfaces;
                self.start = Some(Instant::now());
                self.launched_at = Some(Instant::now());
            }
            Err(e) => {
                self.init_error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let past_grace = self.past_grace();
        let dismiss = self.dismiss;
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                let is_escape = ke.physical_key == PhysicalKey::Code(KeyCode::Escape);
                match dismiss {
                    // Escape always exits, grace or not.
                    DismissMode::EscapeOnly if is_escape => event_loop.exit(),
                    // Any key exits once past the grace window.
                    DismissMode::AnyInput if past_grace => event_loop.exit(),
                    _ => {}
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } if dismiss == DismissMode::AnyInput && past_grace => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_ref()
                    && let Some(surf) = self.surfaces.get_mut(&id)
                {
                    surf.resize(&gpu.device, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let (Some(gpu), Some(start)) = (self.gpu.as_ref(), self.start)
                    && let Some(surf) = self.surfaces.get_mut(&id)
                {
                    surf.render(&gpu.device, &gpu.queue, start.elapsed().as_secs_f32());
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if self.dismiss == DismissMode::AnyInput
            && let DeviceEvent::MouseMotion { delta } = event
            && self.past_grace()
            && delta.0.abs() + delta.1.abs() > 2.0
        {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        for surf in self.surfaces.values() {
            surf.window.request_redraw();
        }
    }
}

/// Run the saver. `shader` is a bundled name, `random`, or a WGSL file path
/// (see [`resolve_shader`]); when `None`, the default bundled effect is used.
/// `dismiss` selects how it exits.
pub fn run(shader: Option<String>, dismiss: DismissMode) -> anyhow::Result<()> {
    let shader_src = resolve_shader(shader.as_deref())?;

    let event_loop = EventLoop::new().context("creating event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        shader_src,
        dismiss,
        ..Default::default()
    };
    event_loop.run_app(&mut app).context("running event loop")?;
    if let Some(e) = app.init_error {
        return Err(e);
    }
    Ok(())
}

/// Render a single frame of `shader` to a PNG at `out`, headlessly — no window
/// or surface, so it works without a display (e.g. in CI for README images).
/// `shader` is a bundled name, `random`, or a WGSL file path (see
/// [`resolve_shader`]), or the bundled default when `None`. `time` picks the
/// moment in the effect's animation to capture (effects are deterministic in
/// `time`). `seed` is the per-shader phase offset; `None` picks a random one each
/// run (logged, so a favourite can be pinned with `--seed`).
pub fn shot(
    shader: Option<String>,
    out: &str,
    width: u32,
    height: u32,
    time: f32,
    seed: Option<f32>,
) -> anyhow::Result<()> {
    let seed = seed.unwrap_or_else(random_seed);
    let shader_src = resolve_shader(shader.as_deref())?;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        // Headless: no surface to be compatible with.
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .context("no suitable GPU adapter")?;

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("scrnsav headless device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .context("requesting GPU device")?;

    // sRGB target so the PNG matches the on-screen (sRGB surface) look; PNG is
    // sRGB-encoded, so the bytes copied back need no further conversion.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // Catch shader-compile / pipeline validation errors as a clean message,
    // mirroring the windowed path instead of panicking.
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("saver shader"),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });
    let bind_group_layout = uniforms_bind_group_layout(&device);
    let pipeline_layout = uniforms_pipeline_layout(&device, &bind_group_layout);
    let pipeline = make_pipeline(&device, &pipeline_layout, &module, format);

    if let Some(err) = pollster::block_on(error_scope.pop()) {
        anyhow::bail!("shader failed to compile:\n{err}");
    }

    let uniforms = Uniforms {
        time,
        seed,
        resolution: [width as f32, height as f32],
    };
    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("uniform buffer"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&uniform_buf, 0, bytemuck::bytes_of(&uniforms));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("uniforms"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }],
    });

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shot target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());

    // copy_texture_to_buffer requires each row padded to a 256-byte multiple.
    let unpadded_bytes_per_row = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shot readback"),
        size: (padded_bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shot pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    // Map the readback buffer and block until the GPU work completes.
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow::anyhow!("polling device: {e:?}"))?;
    rx.recv()
        .context("map_async channel closed")?
        .context("mapping readback buffer")?;

    // Strip the per-row padding into a tight RGBA buffer, forcing alpha opaque
    // so a shader that leaves alpha < 1 still yields a solid screenshot.
    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow::anyhow!("reading mapped buffer: {e:?}"))?;
    let mut pixels = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    for row in 0..height {
        let start = (row * padded_bytes_per_row) as usize;
        let end = start + unpadded_bytes_per_row as usize;
        pixels.extend_from_slice(&data[start..end]);
    }
    drop(data);
    readback.unmap();
    for px in pixels.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }

    image::save_buffer(out, &pixels, width, height, image::ExtendedColorType::Rgba8)
        .with_context(|| format!("writing PNG '{out}'"))?;
    log::info!("wrote {out} ({width}x{height}, --seed {seed})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Uniforms;

    #[test]
    fn uniforms_match_wgsl_layout() {
        // Must stay 16 bytes to match `struct Uniforms` in the WGSL shaders.
        assert_eq!(std::mem::size_of::<Uniforms>(), 16);
    }

    #[test]
    fn resolve_shader_picks_random_bundled() {
        let source = super::resolve_shader(Some("random")).unwrap();
        assert!(super::BUNDLED.iter().any(|(_, src)| *src == source));
    }

    #[test]
    fn resolve_shader_picks_bundled() {
        // None falls back to the default (first bundled = lines), and a bundled
        // name resolves to that effect's source.
        let default = super::resolve_shader(None).unwrap();
        assert_eq!(default, super::resolve_shader(Some("lines")).unwrap());
        assert!(
            super::resolve_shader(Some("plasma"))
                .unwrap()
                .contains("fs_main")
        );
    }
}
