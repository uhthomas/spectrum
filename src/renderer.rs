use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use ab_glyph::{Font, FontArc, ScaleFont, point};
use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    window::Window,
};

use crate::media::{PlaybackUi, SharedMedia, VideoFrame};

const MAX_POINTS: usize = 1024;
const BLUR_DOWNSAMPLE: u32 = 2;
const BLUR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const SETTINGS_PANEL_WIDTH: f32 = 320.0;
const SETTINGS_PANEL_HEIGHT: f32 = 310.0;
const SETTINGS_LABELS: [(&str, f32, f32); 9] = [
    ("Spectrum", 16.0, 13.0),
    ("Enabled", 16.0, 40.0),
    ("Sensitivity", 16.0, 74.0),
    ("Smoothing", 16.0, 108.0),
    ("Spacing", 16.0, 142.0),
    ("Blur", 16.0, 176.0),
    ("Brightness", 16.0, 210.0),
    ("Opacity", 16.0, 244.0),
    ("Reset", 27.0, 280.0),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingKey {
    Sensitivity,
    Smoothing,
    PointSpacing,
    BlurStrength,
    Brightness,
    OverlayOpacity,
}

#[derive(Clone, Copy, Debug)]
pub struct SpectrumSettings {
    pub enabled: bool,
    pub sensitivity: f32,
    pub smoothing: f32,
    pub point_spacing: f32,
    pub blur_strength: f32,
    pub brightness: f32,
    pub overlay_opacity: f32,
}

impl Default for SpectrumSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            sensitivity: 1.0,
            smoothing: 0.67,
            point_spacing: 40.0,
            blur_strength: 40.0,
            brightness: 0.8,
            overlay_opacity: 0.1,
        }
    }
}

impl SpectrumSettings {
    pub fn set_normalized(&mut self, key: SettingKey, value: f32) {
        let value = value.clamp(0.0, 1.0);
        match key {
            SettingKey::Sensitivity => self.sensitivity = 0.5 + value * 1.5,
            SettingKey::Smoothing => self.smoothing = value * 0.95,
            SettingKey::PointSpacing => self.point_spacing = 20.0 + value * 60.0,
            SettingKey::BlurStrength => self.blur_strength = value * 80.0,
            SettingKey::Brightness => self.brightness = 0.4 + value * 0.6,
            SettingKey::OverlayOpacity => self.overlay_opacity = value * 0.3,
        }
    }

    fn normalized(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                (self.sensitivity - 0.5) / 1.5,
                self.smoothing / 0.95,
                (self.point_spacing - 20.0) / 60.0,
                self.blur_strength / 80.0,
            ],
            [
                (self.brightness - 0.4) / 0.6,
                self.overlay_opacity / 0.3,
                u32::from(self.enabled) as f32,
                0.0,
            ],
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    output_size: [f32; 2],
    video_size: [f32; 2],
    content_origin: [f32; 2],
    content_size: [f32; 2],
    blur_size: [f32; 2],
    step_px: f32,
    scale_factor: f32,
    point_count: u32,
    playback_progress: f32,
    volume: f32,
    ui_flags: u32,
    blur_scale: f32,
    filter_brightness: f32,
    filter_opacity: f32,
    _filter_padding: f32,
    settings_a: [f32; 4],
    settings_b: [f32; 4],
}

pub enum ControlAction {
    TogglePlayback,
    Seek(f64),
    ToggleMute,
    Volume(f64),
    ToggleFullscreen,
    ToggleSettings,
    ToggleSpectrum,
    SetSpectrumSetting(SettingKey, f64),
    ResetSpectrumSettings,
    SettingsPanel,
    ControlsBackground,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderControl {
    Seek,
    Volume,
    SpectrumSetting(SettingKey),
}

struct SampledTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    scale_factor: f64,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    composite_texture_layout: wgpu::BindGroupLayout,
    spectrum_buffer: wgpu::Buffer,
    spectrum_bind_group: wgpu::BindGroup,
    video: SampledTexture,
    video_bind_group: wgpu::BindGroup,
    blur_a: SampledTexture,
    blur_a_bind_group: wgpu::BindGroup,
    blur_b: SampledTexture,
    label_font: FontArc,
    label_atlas: SampledTexture,
    composite_texture_bind_group: wgpu::BindGroup,
    horizontal_pipeline: wgpu::RenderPipeline,
    vertical_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    has_video: bool,
    last_spectrum: Vec<f32>,
    stats_enabled: bool,
    stats_started: Instant,
    rendered_frames: u64,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let scale_factor = window.scale_factor();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(Arc::clone(&window))
            .context("failed to create a GPU presentation surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                apply_limit_buckets: false,
            })
            .await
            .context("no compatible GPU adapter was found")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Spectrum device"),
                ..Default::default()
            })
            .await
            .context("failed to create the GPU device")?;

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("the GPU surface is unsupported")?;
        let capabilities = surface.get_capabilities(&adapter);
        if let Some(srgb) = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
        {
            config.format = srgb;
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 2;
        surface.configure(&device, &config);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Spectrum linear sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Spectrum parameters layout"),
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
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Spectrum texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let composite_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Spectrum composite textures layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });
        let spectrum_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Spectrum points layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let initial_params = Params::zeroed();
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Spectrum parameters"),
            contents: bytemuck::bytes_of(&initial_params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Spectrum parameters bind group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let spectrum_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Spectrum curve points"),
            size: (MAX_POINTS * size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let spectrum_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Spectrum curve bind group"),
            layout: &spectrum_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: spectrum_buffer.as_entire_binding(),
            }],
        });

        // GStreamer supplies gamma-encoded sRGB bytes. Keep them encoded so the
        // CSS-compatible Gaussian can average directly in sRGB space.
        let video = create_texture(&device, 1, 1, wgpu::TextureFormat::Rgba8Unorm, "Video");
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &video._texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0, 0, 0, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let video_bind_group =
            make_texture_bind_group(&device, &texture_layout, &video.view, &sampler, "Video");

        let (blur_width, blur_height) = blur_extent(size, scale_factor);
        let blur_a = create_texture(
            &device,
            blur_width,
            blur_height,
            BLUR_FORMAT,
            "Horizontal blur",
        );
        let blur_b = create_texture(
            &device,
            blur_width,
            blur_height,
            BLUR_FORMAT,
            "Vertical blur",
        );
        let blur_a_bind_group = make_texture_bind_group(
            &device,
            &texture_layout,
            &blur_a.view,
            &sampler,
            "Horizontal blur",
        );
        let label_font = load_ui_font()?;
        let label_atlas = create_label_atlas(&device, &queue, &label_font, scale_factor);
        let composite_texture_bind_group = make_composite_texture_bind_group(
            &device,
            &composite_texture_layout,
            &blur_b.view,
            &label_atlas.view,
            &sampler,
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Spectrum shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("spectrum.wgsl").into()),
        });
        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Spectrum blur pipeline layout"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Spectrum composite pipeline layout"),
                bind_group_layouts: &[
                    Some(&uniform_layout),
                    Some(&texture_layout),
                    Some(&composite_texture_layout),
                    Some(&spectrum_layout),
                ],
                immediate_size: 0,
            });
        let horizontal_pipeline = create_pipeline(
            &device,
            &shader,
            &blur_pipeline_layout,
            "blur_horizontal",
            BLUR_FORMAT,
            "Horizontal blur pipeline",
        );
        let vertical_pipeline = create_pipeline(
            &device,
            &shader,
            &blur_pipeline_layout,
            "blur_vertical",
            BLUR_FORMAT,
            "Vertical blur pipeline",
        );
        let composite_pipeline = create_pipeline(
            &device,
            &shader,
            &composite_pipeline_layout,
            "composite",
            config.format,
            "Spectrum composite pipeline",
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            scale_factor,
            sampler,
            uniform_buffer,
            uniform_bind_group,
            texture_layout,
            composite_texture_layout,
            spectrum_buffer,
            spectrum_bind_group,
            video,
            video_bind_group,
            blur_a,
            blur_a_bind_group,
            blur_b,
            label_font,
            label_atlas,
            composite_texture_bind_group,
            horizontal_pipeline,
            vertical_pipeline,
            composite_pipeline,
            has_video: false,
            last_spectrum: Vec::new(),
            stats_enabled: std::env::var_os("SPECTRUM_STATS").is_some(),
            stats_started: Instant::now(),
            rendered_frames: 0,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        self.size = size;
        self.scale_factor = scale_factor;
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.recreate_blur_textures();
    }

    pub fn reset_media(&mut self) {
        self.has_video = false;
        self.last_spectrum.clear();
        self.video = create_texture(
            &self.device,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            "Cleared video",
        );
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.video._texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0, 0, 0, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.video_bind_group = make_texture_bind_group(
            &self.device,
            &self.texture_layout,
            &self.video.view,
            &self.sampler,
            "Cleared video",
        );
    }

    pub fn render(
        &mut self,
        shared: Option<&SharedMedia>,
        playback: Option<PlaybackUi>,
        controls_opacity: f32,
        fullscreen: bool,
        settings_open: bool,
        settings: &SpectrumSettings,
    ) -> Result<()> {
        if self.size.width == 0 || self.size.height == 0 {
            return Ok(());
        }

        if let Some(shared) = shared
            && let Some(frame) = shared.video.lock().unwrap().take()
        {
            self.upload_video(frame);
        }

        let params = self.update_params_and_spectrum(
            shared,
            playback,
            controls_opacity,
            fullscreen,
            settings_open,
            settings,
        );
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&params));

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("GPU surface validation failed")
            }
        };
        let output_view = surface_texture.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Spectrum frame encoder"),
            });

        self.draw_pass(
            &mut encoder,
            &self.blur_a.view,
            &self.horizontal_pipeline,
            &[&self.uniform_bind_group, &self.video_bind_group],
            "Horizontal Gaussian blur",
        );
        self.draw_pass(
            &mut encoder,
            &self.blur_b.view,
            &self.vertical_pipeline,
            &[&self.uniform_bind_group, &self.blur_a_bind_group],
            "Vertical Gaussian blur",
        );
        self.draw_pass(
            &mut encoder,
            &output_view,
            &self.composite_pipeline,
            &[
                &self.uniform_bind_group,
                &self.video_bind_group,
                &self.composite_texture_bind_group,
                &self.spectrum_bind_group,
            ],
            "Spectrum composite",
        );

        self.queue.submit([encoder.finish()]);
        self.queue.present(surface_texture);
        self.report_stats(shared);
        Ok(())
    }

    pub fn control_action(
        &self,
        position: PhysicalPosition<f64>,
        settings_open: bool,
    ) -> Option<ControlAction> {
        let scale = self.scale_factor;
        let width = self.size.width as f64 / scale;
        let height = self.size.height as f64 / scale;
        let x = position.x / scale;
        let y = position.y / scale;

        if settings_open {
            let panel_right = width - 16.0;
            let panel_left = (panel_right - 320.0).max(16.0);
            let panel_bottom = height - 84.0;
            let panel_top = panel_bottom - 310.0;
            if (panel_left..=panel_right).contains(&x) && (panel_top..=panel_bottom).contains(&y) {
                if (panel_top + 34.0..=panel_top + 62.0).contains(&y) {
                    return Some(ControlAction::ToggleSpectrum);
                }

                let slider_start = panel_left + 140.0;
                let slider_end = panel_right - 20.0;
                let slider_value =
                    || ((x - slider_start) / (slider_end - slider_start)).clamp(0.0, 1.0);
                let rows = [
                    (panel_top + 82.0, SettingKey::Sensitivity),
                    (panel_top + 116.0, SettingKey::Smoothing),
                    (panel_top + 150.0, SettingKey::PointSpacing),
                    (panel_top + 184.0, SettingKey::BlurStrength),
                    (panel_top + 218.0, SettingKey::Brightness),
                    (panel_top + 252.0, SettingKey::OverlayOpacity),
                ];
                for (center, key) in rows {
                    if (center - 14.0..=center + 14.0).contains(&y) && x >= slider_start - 8.0 {
                        return Some(ControlAction::SetSpectrumSetting(key, slider_value()));
                    }
                }

                if (panel_left + 16.0..=panel_left + 96.0).contains(&x)
                    && (panel_top + 274.0..=panel_top + 302.0).contains(&y)
                {
                    return Some(ControlAction::ResetSpectrumSettings);
                }
                return Some(ControlAction::SettingsPanel);
            }
        }

        if y < height - 72.0 {
            return None;
        }

        if (16.0..=56.0).contains(&x) && (height - 58.0..=height - 18.0).contains(&y) {
            return Some(ControlAction::TogglePlayback);
        }

        let seek_start = 76.0;
        let seek_end = (width - 286.0).max(seek_start + 20.0);
        if (seek_start..=seek_end).contains(&x) && (height - 52.0..=height - 20.0).contains(&y) {
            return Some(ControlAction::Seek(
                ((x - seek_start) / (seek_end - seek_start)).clamp(0.0, 1.0),
            ));
        }

        let volume_start = (width - 220.0).max(seek_end + 30.0);
        let volume_end = (width - 108.0).max(volume_start + 20.0);
        if (volume_start - 32.0..=volume_start - 4.0).contains(&x)
            && (height - 52.0..=height - 20.0).contains(&y)
        {
            return Some(ControlAction::ToggleMute);
        }
        if (volume_start..=volume_end).contains(&x) && (height - 52.0..=height - 20.0).contains(&y)
        {
            return Some(ControlAction::Volume(
                ((x - volume_start) / (volume_end - volume_start)).clamp(0.0, 1.0),
            ));
        }

        if (width - 88.0..=width - 50.0).contains(&x)
            && (height - 58.0..=height - 18.0).contains(&y)
        {
            return Some(ControlAction::ToggleSettings);
        }
        if (width - 50.0..=width - 8.0).contains(&x) && (height - 58.0..=height - 18.0).contains(&y)
        {
            return Some(ControlAction::ToggleFullscreen);
        }

        Some(ControlAction::ControlsBackground)
    }

    pub fn drag_action(
        &self,
        position: PhysicalPosition<f64>,
        control: SliderControl,
    ) -> ControlAction {
        let scale = self.scale_factor;
        let width = self.size.width as f64 / scale;
        let x = position.x / scale;

        match control {
            SliderControl::Seek => {
                let start = 76.0;
                let end = (width - 286.0).max(start + 20.0);
                ControlAction::Seek(((x - start) / (end - start)).clamp(0.0, 1.0))
            }
            SliderControl::Volume => {
                let seek_end = (width - 286.0).max(96.0);
                let start = (width - 220.0).max(seek_end + 30.0);
                let end = (width - 108.0).max(start + 20.0);
                ControlAction::Volume(((x - start) / (end - start)).clamp(0.0, 1.0))
            }
            SliderControl::SpectrumSetting(key) => {
                let panel_right = width - 16.0;
                let panel_left = (panel_right - 320.0).max(16.0);
                let start = panel_left + 140.0;
                let end = panel_right - 20.0;
                ControlAction::SetSpectrumSetting(
                    key,
                    ((x - start) / (end - start)).clamp(0.0, 1.0),
                )
            }
        }
    }

    fn upload_video(&mut self, frame: VideoFrame) {
        if self.video.width != frame.width || self.video.height != frame.height {
            self.video = create_texture(
                &self.device,
                frame.width,
                frame.height,
                wgpu::TextureFormat::Rgba8Unorm,
                "Video",
            );
            self.video_bind_group = make_texture_bind_group(
                &self.device,
                &self.texture_layout,
                &self.video.view,
                &self.sampler,
                "Video",
            );
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.video._texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
        self.has_video = true;
    }

    fn update_params_and_spectrum(
        &mut self,
        shared: Option<&SharedMedia>,
        playback: Option<PlaybackUi>,
        controls_opacity: f32,
        fullscreen: bool,
        settings_open: bool,
        settings: &SpectrumSettings,
    ) -> Params {
        let output_width = self.size.width as f32;
        let output_height = self.size.height as f32;
        let dpr = self.scale_factor as f32;
        let mut origin = [0.0, 0.0];
        let mut content = [0.0, 0.0];
        let mut point_count = 0;
        let step_px = settings.point_spacing * dpr * dpr;

        if self.has_video {
            let logical_width = output_width / dpr;
            let logical_height = output_height / dpr;
            let video_ratio = self.video.width as f32 / self.video.height as f32;
            let window_ratio = logical_width / logical_height;
            let content_width = if window_ratio > video_ratio {
                logical_height * video_ratio
            } else {
                logical_width
            };
            let content_height = if window_ratio < video_ratio {
                logical_width / video_ratio
            } else {
                logical_height
            };
            origin = [
                ((logical_width - content_width) * 0.5).round() * dpr,
                ((logical_height - content_height) * 0.5).round() * dpr,
            ];
            content = [content_width * dpr, content_height * dpr];
            point_count = ((content_width / (settings.point_spacing * dpr) + 2.0).round() as usize)
                .clamp(2, MAX_POINTS);
        } else if shared.is_some() {
            origin = [0.0, 0.0];
            content = [output_width, output_height];
            let logical_width = output_width / dpr;
            point_count = ((logical_width / (settings.point_spacing * dpr) + 2.0).round() as usize)
                .clamp(2, MAX_POINTS);
        }

        if !settings.enabled {
            point_count = 0;
        }

        if point_count >= 2 {
            if let Some(shared) = shared {
                let db = shared
                    .analyser
                    .lock()
                    .unwrap()
                    .frequency_data(point_count, settings.smoothing);
                if db.len() == point_count {
                    self.last_spectrum = db
                        .into_iter()
                        .map(|value| {
                            if value.is_finite() {
                                (content[1]
                                    - (value + 66.0) * 9.75 * settings.sensitivity * dpr * dpr)
                                    .max(0.0)
                            } else {
                                content[1]
                            }
                        })
                        .collect();
                }
            }
            if self.last_spectrum.len() == point_count {
                self.queue.write_buffer(
                    &self.spectrum_buffer,
                    0,
                    bytemuck::cast_slice(&self.last_spectrum),
                );
            } else {
                point_count = 0;
            }
        }

        let playback_progress = playback
            .filter(|state| state.duration > 0.0)
            .map_or(0.0, |state| {
                (state.position / state.duration).clamp(0.0, 1.0) as f32
            });
        let volume = playback.map_or(0.0, |state| state.volume.clamp(0.0, 1.0) as f32);
        let mut ui_flags = 0;
        if let Some(playback) = playback {
            ui_flags |= 2;
            if playback.playing {
                ui_flags |= 1;
            }
            if playback.muted {
                ui_flags |= 4;
            }
        }
        ui_flags |= ((controls_opacity.clamp(0.0, 1.0) * 255.0).round() as u32) << 8;
        if fullscreen {
            ui_flags |= 8;
        }
        if settings_open {
            ui_flags |= 16;
        }
        if settings.enabled {
            ui_flags |= 32;
        }
        let (settings_a, settings_b) = settings.normalized();

        Params {
            output_size: [output_width, output_height],
            video_size: if self.has_video {
                [self.video.width as f32, self.video.height as f32]
            } else {
                [0.0, 0.0]
            },
            content_origin: origin,
            content_size: content,
            blur_size: [self.blur_a.width as f32, self.blur_a.height as f32],
            step_px,
            scale_factor: dpr,
            point_count: point_count as u32,
            playback_progress,
            volume,
            ui_flags,
            blur_scale: settings.blur_strength / 40.0,
            filter_brightness: settings.brightness,
            filter_opacity: settings.overlay_opacity,
            _filter_padding: 0.0,
            settings_a,
            settings_b,
        }
    }

    fn recreate_blur_textures(&mut self) {
        let (width, height) = blur_extent(self.size, self.scale_factor);
        self.blur_a = create_texture(&self.device, width, height, BLUR_FORMAT, "Horizontal blur");
        self.blur_b = create_texture(&self.device, width, height, BLUR_FORMAT, "Vertical blur");
        self.blur_a_bind_group = make_texture_bind_group(
            &self.device,
            &self.texture_layout,
            &self.blur_a.view,
            &self.sampler,
            "Horizontal blur",
        );

        let (label_width, label_height) = label_atlas_extent(self.scale_factor);
        if self.label_atlas.width != label_width || self.label_atlas.height != label_height {
            self.label_atlas = create_label_atlas(
                &self.device,
                &self.queue,
                &self.label_font,
                self.scale_factor,
            );
        }
        self.composite_texture_bind_group = make_composite_texture_bind_group(
            &self.device,
            &self.composite_texture_layout,
            &self.blur_b.view,
            &self.label_atlas.view,
            &self.sampler,
        );
    }

    fn report_stats(&mut self, shared: Option<&SharedMedia>) {
        if !self.stats_enabled {
            return;
        }

        self.rendered_frames += 1;
        let elapsed = self.stats_started.elapsed();
        if elapsed.as_secs_f64() < 1.0 {
            return;
        }

        let seconds = elapsed.as_secs_f64();
        let (video_frames, audio_frames) = shared.map_or((0, 0), |shared| {
            (
                shared.decoded_video_frames.swap(0, Ordering::Relaxed),
                shared.analysed_audio_frames.swap(0, Ordering::Relaxed),
            )
        });
        eprintln!(
            "render {:.1} fps, video {:.1} fps, audio {:.0} frames/s",
            self.rendered_frames as f64 / seconds,
            video_frames as f64 / seconds,
            audio_frames as f64 / seconds,
        );
        self.rendered_frames = 0;
        self.stats_started = Instant::now();
    }

    fn draw_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        bind_groups: &[&wgpu::BindGroup],
        label: &'static str,
    ) {
        let attachment = wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(attachment)],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        for (index, group) in bind_groups.iter().enumerate() {
            pass.set_bind_group(index as u32, Some(*group), &[]);
        }
        pass.draw(0..3, 0..1);
    }
}

fn load_ui_font() -> Result<FontArc> {
    let mut candidates = Vec::<PathBuf>::new();
    if let Some(path) = std::env::var_os("SPECTRUM_FONT") {
        candidates.push(path.into());
    }

    #[cfg(target_os = "linux")]
    if let Ok(output) = Command::new("fc-match")
        .args(["-f", "%{file}\\n", "sans-serif"])
        .output()
        && output.status.success()
        && let Some(path) = String::from_utf8_lossy(&output.stdout).lines().next()
        && !path.is_empty()
    {
        candidates.push(path.into());
    }

    #[cfg(target_os = "linux")]
    candidates.extend([
        PathBuf::from("/usr/share/fonts/noto/NotoSans-Regular.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        PathBuf::from("/usr/share/fonts/liberation/LiberationSans-Regular.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf"),
    ]);
    #[cfg(target_os = "macos")]
    candidates.push(PathBuf::from("/System/Library/Fonts/SFNS.ttf"));
    #[cfg(target_os = "windows")]
    candidates.push(PathBuf::from(r"C:\Windows\Fonts\segoeui.ttf"));

    for path in candidates {
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(font) = FontArc::try_from_vec(bytes)
        {
            return Ok(font);
        }
    }

    anyhow::bail!(
        "no usable system UI font was found; set SPECTRUM_FONT to a TrueType or OpenType font"
    )
}

fn label_atlas_extent(scale_factor: f64) -> (u32, u32) {
    (
        (f64::from(SETTINGS_PANEL_WIDTH) * scale_factor)
            .ceil()
            .max(1.0) as u32,
        (f64::from(SETTINGS_PANEL_HEIGHT) * scale_factor)
            .ceil()
            .max(1.0) as u32,
    )
}

fn rasterize_label_atlas(font: &FontArc, scale_factor: f64) -> (u32, u32, Vec<u8>) {
    let (width, height) = label_atlas_extent(scale_factor);
    let mut alpha = vec![0; width as usize * height as usize];
    let dpr = scale_factor as f32;
    let font_size = 13.0 * dpr;
    let scaled_font = font.as_scaled(font_size);

    for (text, logical_x, logical_y) in SETTINGS_LABELS {
        let mut caret = logical_x * dpr;
        let baseline = logical_y * dpr + scaled_font.ascent();
        let mut previous = None;
        for character in text.chars() {
            let glyph_id = scaled_font.glyph_id(character);
            if let Some(previous) = previous {
                caret += scaled_font.kern(previous, glyph_id);
            }
            let glyph = glyph_id.with_scale_and_position(font_size, point(caret, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|x, y, coverage| {
                    let x = bounds.min.x as i32 + x as i32;
                    let y = bounds.min.y as i32 + y as i32;
                    if x >= 0 && y >= 0 && x < width as i32 && y < height as i32 {
                        let index = y as usize * width as usize + x as usize;
                        alpha[index] = alpha[index].max((coverage * 255.0).round() as u8);
                    }
                });
            }
            caret += scaled_font.h_advance(glyph_id);
            previous = Some(glyph_id);
        }
    }

    (width, height, alpha)
}

fn create_label_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &FontArc,
    scale_factor: f64,
) -> SampledTexture {
    let (width, height, alpha) = rasterize_label_atlas(font, scale_factor);
    let texture = create_texture(
        device,
        width,
        height,
        wgpu::TextureFormat::R8Unorm,
        "Settings label atlas",
    );
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture._texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &alpha,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn create_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> SampledTexture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    SampledTexture {
        _texture: texture,
        view,
        width,
        height,
    }
}

fn blur_extent(size: PhysicalSize<u32>, scale_factor: f64) -> (u32, u32) {
    // Keep the intermediate texture at half the logical resolution. Twenty
    // blur texels therefore remain equal to CSS blur(40px) on HiDPI output.
    let divisor = BLUR_DOWNSAMPLE as f64 * scale_factor;
    (
        (size.width as f64 / divisor).ceil().max(1.0) as u32,
        (size.height as f64 / divisor).ceil().max(1.0) as u32,
    )
}

fn make_texture_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    label: &'static str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn make_composite_texture_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    blur_view: &wgpu::TextureView,
    label_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Spectrum composite textures"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(blur_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(label_view),
            },
        ],
    })
}

fn create_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    fragment_entry: &'static str,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("fullscreen"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approximately(left: f32, right: f32) {
        assert!((left - right).abs() < 0.000_5, "{left} != {right}");
    }

    #[test]
    fn defaults_preserve_the_browser_preset() {
        let settings = SpectrumSettings::default();
        assert!(settings.enabled);
        approximately(settings.sensitivity, 1.0);
        approximately(settings.smoothing, 0.67);
        approximately(settings.point_spacing, 40.0);
        approximately(settings.blur_strength, 40.0);
        approximately(settings.brightness, 0.8);
        approximately(settings.overlay_opacity, 0.1);
    }

    #[test]
    fn normalized_settings_reach_their_documented_limits() {
        let mut settings = SpectrumSettings::default();
        settings.set_normalized(SettingKey::Sensitivity, 0.0);
        settings.set_normalized(SettingKey::Smoothing, 1.0);
        settings.set_normalized(SettingKey::PointSpacing, 1.0);
        settings.set_normalized(SettingKey::BlurStrength, 0.0);
        settings.set_normalized(SettingKey::Brightness, 0.0);
        settings.set_normalized(SettingKey::OverlayOpacity, 1.0);
        approximately(settings.sensitivity, 0.5);
        approximately(settings.smoothing, 0.95);
        approximately(settings.point_spacing, 80.0);
        approximately(settings.blur_strength, 0.0);
        approximately(settings.brightness, 0.4);
        approximately(settings.overlay_opacity, 0.3);
    }
}
