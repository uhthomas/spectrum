mod analyser;
mod media;
mod renderer;
#[cfg(target_os = "linux")]
mod wayland_drop;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use media::{MediaPlayer, file_uri, media_title, uri_from_argument};
use renderer::{ControlAction, Renderer, SettingKey, SliderControl, SpectrumSettings};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId},
};

const CONTROLS_VISIBLE_FOR: Duration = Duration::from_millis(2_500);
const CONTROLS_FADE_TIME: Duration = Duration::from_millis(350);

fn main() -> Result<()> {
    if std::env::args_os().any(|argument| argument == "--help" || argument == "-h") {
        println!(
            "Usage: spectrum-native [MEDIA_FILE_OR_HTTP_URL]\n\nDrop another file or HTTP URL onto the window to load it.\nSpace: play/pause  Left/Right: seek 5s  F: fullscreen  Esc: quit"
        );
        return Ok(());
    }

    gstreamer::init().context("failed to initialize GStreamer")?;
    let initial_uri = std::env::args_os()
        .nth(1)
        .map(uri_from_argument)
        .transpose()?;
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("failed to create the native event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(initial_uri, event_loop.create_proxy());
    event_loop
        .run_app(&mut app)
        .context("native event loop failed")
}

#[derive(Debug)]
pub(crate) enum UserEvent {
    DroppedMedia(url::Url),
    DropError(String),
}

#[derive(Clone, Copy, PartialEq)]
enum DragControl {
    Seek(f64),
    Volume,
    SpectrumSetting(SettingKey),
}

impl DragControl {
    fn slider(self) -> SliderControl {
        match self {
            Self::Seek(_) => SliderControl::Seek,
            Self::Volume => SliderControl::Volume,
            Self::SpectrumSetting(key) => SliderControl::SpectrumSetting(key),
        }
    }
}

struct App {
    initial_uri: Option<url::Url>,
    event_proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    media: Option<MediaPlayer>,
    cursor_position: Option<PhysicalPosition<f64>>,
    pointer_down: bool,
    dragging_control: Option<DragControl>,
    controls_hide_at: Instant,
    settings_open: bool,
    settings: SpectrumSettings,
    #[cfg(target_os = "linux")]
    wayland_drop: Option<wayland_drop::WaylandDrop>,
}

impl App {
    fn new(
        initial_uri: Option<url::Url>,
        event_proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) -> Self {
        Self {
            initial_uri,
            event_proxy,
            window: None,
            renderer: None,
            media: None,
            cursor_position: None,
            pointer_down: false,
            dragging_control: None,
            controls_hide_at: Instant::now() + CONTROLS_VISIBLE_FOR,
            settings_open: false,
            settings: SpectrumSettings::default(),
            #[cfg(target_os = "linux")]
            wayland_drop: None,
        }
    }

    fn load(&mut self, uri: &url::Url) {
        match MediaPlayer::open(uri) {
            Ok(player) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.reset_media();
                }
                self.media = Some(player);
                if let Some(window) = &self.window {
                    window.set_title(&format!("Spectrum — {}", media_title(uri)));
                }
            }
            Err(error) => eprintln!("Could not load {uri}: {error:#}"),
        }
    }

    fn activate_control(&mut self, position: PhysicalPosition<f64>, is_press: bool) -> bool {
        let Some(renderer) = self.renderer.as_ref() else {
            return false;
        };
        let action = if is_press {
            renderer.control_action(position, self.settings_open)
        } else {
            self.dragging_control
                .map(|drag| renderer.drag_action(position, drag.slider()))
        };
        let Some(action) = action else {
            return false;
        };
        if is_press {
            self.dragging_control = match &action {
                ControlAction::Seek(fraction) => Some(DragControl::Seek(*fraction)),
                ControlAction::Volume(_) => Some(DragControl::Volume),
                ControlAction::SetSpectrumSetting(key, _) => {
                    Some(DragControl::SpectrumSetting(*key))
                }
                _ => None,
            };
        }
        if is_press && matches!(&action, ControlAction::ToggleFullscreen) {
            self.toggle_fullscreen();
            return true;
        }
        match &action {
            ControlAction::ToggleSettings if is_press => {
                self.settings_open = !self.settings_open;
                return true;
            }
            ControlAction::ToggleSpectrum if is_press => {
                self.settings.enabled = !self.settings.enabled;
                return true;
            }
            ControlAction::SetSpectrumSetting(key, value) => {
                self.settings.set_normalized(*key, *value as f32);
                return true;
            }
            ControlAction::ResetSpectrumSettings if is_press => {
                self.settings = SpectrumSettings::default();
                return true;
            }
            ControlAction::SettingsPanel | ControlAction::ControlsBackground => return true,
            _ => {}
        }
        if let ControlAction::Seek(fraction) = &action {
            if let Some(DragControl::Seek(preview)) = &mut self.dragging_control {
                *preview = *fraction;
            }
            return true;
        }
        let Some(media) = &mut self.media else {
            return true;
        };
        match action {
            ControlAction::TogglePlayback if is_press => {
                if let Err(error) = media.toggle_pause() {
                    eprintln!("Could not change playback state: {error:#}");
                }
            }
            ControlAction::ToggleMute if is_press => media.toggle_mute(),
            ControlAction::Volume(volume) => media.set_volume(volume),
            _ => {}
        }
        true
    }

    fn toggle_playback(&mut self) {
        if let Some(media) = &mut self.media
            && let Err(error) = media.toggle_pause()
        {
            eprintln!("Could not change playback state: {error:#}");
        }
    }

    fn toggle_fullscreen(&self) {
        if let Some(window) = &self.window {
            let fullscreen = if window.fullscreen().is_some() {
                None
            } else {
                Some(Fullscreen::Borderless(None))
            };
            window.set_fullscreen(fullscreen);
        }
    }

    fn show_controls(&mut self) {
        self.controls_hide_at = Instant::now() + CONTROLS_VISIBLE_FOR;
    }

    fn controls_opacity(&self, playing: bool) -> f32 {
        if self.pointer_down || self.settings_open || !playing {
            return 1.0;
        }
        let remaining = self
            .controls_hide_at
            .saturating_duration_since(Instant::now());
        (remaining.as_secs_f32() / CONTROLS_FADE_TIME.as_secs_f32()).clamp(0.0, 1.0)
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = WindowAttributes::default()
            .with_title("Spectrum — drop a media file or HTTP URL here")
            .with_inner_size(LogicalSize::new(1280.0, 720.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("Failed to create the native window: {error}");
                event_loop.exit();
                return;
            }
        };
        match pollster::block_on(Renderer::new(Arc::clone(&window))) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                eprintln!("Failed to initialize the GPU renderer: {error:#}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);

        #[cfg(target_os = "linux")]
        if let Some(window) = &self.window {
            match wayland_drop::WaylandDrop::new(window, self.event_proxy.clone()) {
                Ok(drop) => self.wayland_drop = drop,
                Err(error) => {
                    eprintln!("Could not initialize native Wayland file drops: {error:#}")
                }
            }
        }

        if let Some(uri) = self.initial_uri.take() {
            self.load(&uri);
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::DroppedMedia(uri) => self.load(&uri),
            UserEvent::DropError(error) => eprintln!("Could not read dropped media: {error}"),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size, window.scale_factor());
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(window.inner_size(), scale_factor);
                }
            }
            WindowEvent::DroppedFile(path) => match file_uri(&path) {
                Ok(uri) => self.load(&uri),
                Err(error) => eprintln!("Could not load {}: {error:#}", path.display()),
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.show_controls();
                self.cursor_position = Some(position);
                let over_control = self.media.is_some()
                    && self
                        .renderer
                        .as_ref()
                        .and_then(|renderer| renderer.control_action(position, self.settings_open))
                        .is_some();
                window.set_cursor(if over_control {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                });
                if self.pointer_down && self.dragging_control.is_some() {
                    self.activate_control(position, false);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let controls_were_visible = self
                    .controls_opacity(self.media.as_ref().is_some_and(MediaPlayer::is_playing))
                    > 0.0;
                self.show_controls();
                self.pointer_down = state == ElementState::Pressed;
                if self.pointer_down {
                    self.dragging_control = None;
                    let handled = controls_were_visible
                        && self
                            .cursor_position
                            .is_some_and(|position| self.activate_control(position, true));
                    if !handled {
                        self.toggle_playback();
                    }
                } else {
                    if let Some(DragControl::Seek(fraction)) = self.dragging_control.take()
                        && let Some(media) = &mut self.media
                        && let Err(error) = media.seek_fraction(fraction)
                    {
                        eprintln!("Could not seek: {error:#}");
                    }
                    self.dragging_control = None;
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => {
                self.show_controls();
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::Space) => {
                        self.toggle_playback();
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if let Some(media) = &mut self.media
                            && let Err(error) = media.seek_relative(-5, event.repeat)
                        {
                            eprintln!("Could not seek: {error:#}");
                        }
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        if let Some(media) = &mut self.media
                            && let Err(error) = media.seek_relative(5, event.repeat)
                        {
                            eprintln!("Could not seek: {error:#}");
                        }
                    }
                    Key::Character(character) if character.eq_ignore_ascii_case("f") => {
                        self.toggle_fullscreen();
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(media) = &self.media
                    && let Some(message) = media.poll_bus()
                {
                    eprintln!("{message}");
                }
                let shared = self.media.as_ref().map(|media| media.shared.as_ref());
                let mut playback = self.media.as_ref().map(MediaPlayer::update);
                if let (Some(DragControl::Seek(fraction)), Some(playback)) =
                    (self.dragging_control, &mut playback)
                {
                    playback.position = playback.duration * fraction;
                }
                let controls_opacity =
                    self.controls_opacity(self.media.as_ref().is_some_and(MediaPlayer::is_playing));
                if controls_opacity <= 0.0 {
                    window.set_cursor(CursorIcon::Default);
                }
                let fullscreen = window.fullscreen().is_some();
                if let Some(renderer) = &mut self.renderer
                    && let Err(error) = renderer.render(
                        shared,
                        playback,
                        controls_opacity,
                        fullscreen,
                        self.settings_open,
                        &self.settings,
                    )
                {
                    eprintln!("Render failed: {error:#}");
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "linux")]
        if let Some(drop) = &mut self.wayland_drop
            && let Err(error) = drop.dispatch_pending()
        {
            eprintln!("Wayland file-drop dispatch failed: {error:#}");
            self.wayland_drop = None;
        }

        if let Some(window) = &self.window {
            window.request_redraw();
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_micros(16_667),
            ));
        }
    }
}
