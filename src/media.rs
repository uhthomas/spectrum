use std::{
    collections::VecDeque,
    ffi::OsString,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use crate::analyser::Analyser;

pub(crate) fn uri_from_argument(argument: OsString) -> Result<url::Url> {
    if let Some(argument) = argument.to_str()
        && (argument
            .get(.."http://".len())
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
            || argument
                .get(.."https://".len())
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://")))
    {
        return url::Url::parse(argument)
            .with_context(|| format!("invalid HTTP media URL: {argument}"));
    }

    file_uri(Path::new(&argument))
}

pub(crate) fn file_uri(path: &Path) -> Result<url::Url> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    url::Url::from_file_path(&canonical)
        .map_err(|_| anyhow!("failed to construct a file URI for {}", canonical.display()))
}

pub(crate) fn media_title(uri: &url::Url) -> String {
    if let Ok(path) = uri.to_file_path() {
        return path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
    }

    uri.path_segments()
        .and_then(|segments| segments.rev().find(|segment| !segment.is_empty()))
        .or_else(|| uri.host_str())
        .unwrap_or("HTTP media")
        .to_owned()
}

#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

struct AudioChunk {
    stream_time_ns: u64,
    sample_rate: u32,
    channels: usize,
    frames: usize,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct SharedMedia {
    pub video: Mutex<Option<VideoFrame>>,
    pub analyser: Mutex<Analyser>,
    pending_audio: Mutex<VecDeque<AudioChunk>>,
    reset_analyser: AtomicBool,
    pub decoded_video_frames: AtomicU64,
    pub analysed_audio_frames: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlaybackUi {
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
    pub playing: bool,
    pub muted: bool,
}

pub struct MediaPlayer {
    playbin: gst::Element,
    pub shared: Arc<SharedMedia>,
    playing: bool,
    relative_seek_target: Option<PendingSeek>,
    analysis_updated_at: Mutex<Instant>,
}

#[derive(Clone, Copy)]
struct PendingSeek {
    target_ns: u64,
    requested_at: Instant,
}

const SEEK_SEQUENCE_GRACE: Duration = Duration::from_millis(250);

impl MediaPlayer {
    pub fn open(uri: &url::Url) -> Result<Self> {
        let shared = Arc::new(SharedMedia::default());
        let video_sink = make_video_sink(Arc::clone(&shared));
        let audio_filter = make_audio_filter(Arc::clone(&shared))?;

        let playbin = gst::ElementFactory::make("playbin3")
            .name("spectrum-player")
            .build()
            .context("the GStreamer playbin3 plugin is unavailable")?;
        playbin.set_property("uri", uri.as_str());
        playbin.set_property("video-sink", &video_sink);
        playbin.set_property("audio-filter", &audio_filter);
        playbin
            .set_state(gst::State::Playing)
            .map_err(|error| anyhow!("failed to start playback: {error:?}"))?;

        Ok(Self {
            playbin,
            shared,
            playing: true,
            relative_seek_target: None,
            analysis_updated_at: Mutex::new(Instant::now()),
        })
    }

    pub fn toggle_pause(&mut self) -> Result<()> {
        self.playing = !self.playing;
        let state = if self.playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        self.playbin
            .set_state(state)
            .map_err(|error| anyhow!("failed to change playback state: {error:?}"))?;
        *self.analysis_updated_at.lock().unwrap() = Instant::now();
        Ok(())
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn seek_relative(&mut self, seconds: i64, repeated: bool) -> Result<()> {
        let current = self
            .playbin
            .query_position::<gst::ClockTime>()
            .unwrap_or(gst::ClockTime::ZERO);
        let now = Instant::now();
        let pending_target = self
            .relative_seek_target
            .filter(|pending| {
                repeated
                    || now.saturating_duration_since(pending.requested_at) <= SEEK_SEQUENCE_GRACE
            })
            .map(|pending| pending.target_ns);
        let duration_ns = self
            .playbin
            .query_duration::<gst::ClockTime>()
            .map(gst::ClockTime::nseconds);
        let target_ns =
            relative_seek_target(current.nseconds(), pending_target, seconds, duration_ns);
        self.playbin
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(target_ns),
            )
            .context("seek failed")?;
        self.relative_seek_target = Some(PendingSeek {
            target_ns,
            requested_at: now,
        });
        self.reset_analysis();
        Ok(())
    }

    pub fn seek_fraction(&mut self, fraction: f64) -> Result<()> {
        let duration = self
            .playbin
            .query_duration::<gst::ClockTime>()
            .context("the media duration is not available yet")?;
        let target = duration.nseconds() as f64 * fraction.clamp(0.0, 1.0);
        self.playbin
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(target as u64),
            )
            .context("seek failed")?;
        self.relative_seek_target = Some(PendingSeek {
            target_ns: target as u64,
            requested_at: Instant::now(),
        });
        self.reset_analysis();
        Ok(())
    }

    pub fn set_volume(&self, volume: f64) {
        self.playbin.set_property("volume", volume.clamp(0.0, 1.0));
    }

    pub fn toggle_mute(&self) {
        let muted = self.playbin.property::<bool>("mute");
        self.playbin.set_property("mute", !muted);
    }

    pub fn update(&self) -> PlaybackUi {
        let position = self
            .playbin
            .query_position::<gst::ClockTime>()
            .unwrap_or(gst::ClockTime::ZERO);
        let duration = self
            .playbin
            .query_duration::<gst::ClockTime>()
            .unwrap_or(gst::ClockTime::ZERO);
        self.advance_analysis(position);
        self.advance_paused_silence();

        PlaybackUi {
            position: position.seconds_f64(),
            duration: duration.seconds_f64(),
            volume: self.playbin.property::<f64>("volume"),
            playing: self.playing,
            muted: self.playbin.property::<bool>("mute"),
        }
    }

    pub fn poll_bus(&self) -> Option<String> {
        let bus = self.playbin.bus()?;
        while let Some(message) = bus.pop() {
            use gst::MessageView;
            match message.view() {
                MessageView::Error(error) => {
                    return Some(format!(
                        "GStreamer error from {}: {} ({:?})",
                        error
                            .src()
                            .map(|source| source.path_string())
                            .unwrap_or_default(),
                        error.error(),
                        error.debug()
                    ));
                }
                MessageView::Eos(..) => return Some("Playback finished".to_owned()),
                _ => {}
            }
        }
        None
    }

    fn reset_analysis(&self) {
        self.shared.pending_audio.lock().unwrap().clear();
        self.shared.reset_analyser.store(true, Ordering::Release);
    }

    fn advance_analysis(&self, position: gst::ClockTime) {
        let reset = self.shared.reset_analyser.swap(false, Ordering::AcqRel);
        let cutoff = position
            .checked_add(gst::ClockTime::from_mseconds(25))
            .unwrap_or(gst::ClockTime::MAX)
            .nseconds();
        let mut ready = Vec::new();
        {
            let mut pending = self.shared.pending_audio.lock().unwrap();
            while pending
                .front()
                .is_some_and(|chunk| chunk.stream_time_ns <= cutoff)
            {
                ready.push(pending.pop_front().unwrap());
            }
        }

        if reset || !ready.is_empty() {
            let mut analyser = self.shared.analyser.lock().unwrap();
            if reset {
                analyser.reset();
            }
            for chunk in ready {
                analyser.push_interleaved_f32le(&chunk.bytes, chunk.channels, chunk.sample_rate);
                self.shared
                    .analysed_audio_frames
                    .fetch_add(chunk.frames as u64, Ordering::Relaxed);
            }
        }
    }

    fn advance_paused_silence(&self) {
        let now = Instant::now();
        let elapsed = {
            let mut updated_at = self.analysis_updated_at.lock().unwrap();
            let elapsed = now.saturating_duration_since(*updated_at);
            *updated_at = now;
            elapsed.min(Duration::from_millis(100))
        };
        if self.playing {
            return;
        }

        let mut analyser = self.shared.analyser.lock().unwrap();
        let frames = (elapsed.as_secs_f64() * analyser.sample_rate() as f64).round() as usize;
        analyser.push_silence(frames);
    }
}

fn relative_seek_target(
    current_ns: u64,
    pending_target_ns: Option<u64>,
    seconds: i64,
    duration_ns: Option<u64>,
) -> u64 {
    let base_ns = pending_target_ns.unwrap_or(current_ns) as i128;
    let delta_ns = seconds as i128 * 1_000_000_000;
    (base_ns + delta_ns).clamp(0, duration_ns.unwrap_or(u64::MAX) as i128) as u64
}

impl Drop for MediaPlayer {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
    }
}

fn make_video_sink(shared: Arc<SharedMedia>) -> gst_app::AppSink {
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();

    let callbacks = gst_app::AppSinkCallbacks::builder()
        .new_sample(move |sink| {
            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
            let caps = sample.caps().ok_or(gst::FlowError::NotNegotiated)?;
            let info =
                gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::NotNegotiated)?;
            let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
            let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
                .map_err(|_| gst::FlowError::Error)?;
            let data = frame.plane_data(0).map_err(|_| gst::FlowError::Error)?;

            let width = info.width() as usize;
            let height = info.height() as usize;
            let stride = info.stride()[0];
            if stride < 0 || (stride as usize) < width * 4 {
                return Err(gst::FlowError::Error);
            }

            let stride = stride as usize;
            let mut rgba = vec![0; width * height * 4];
            for row in 0..height {
                let source = &data[row * stride..row * stride + width * 4];
                rgba[row * width * 4..(row + 1) * width * 4].copy_from_slice(source);
            }

            *shared.video.lock().unwrap() = Some(VideoFrame {
                width: width as u32,
                height: height as u32,
                rgba,
            });
            shared.decoded_video_frames.fetch_add(1, Ordering::Relaxed);
            Ok(gst::FlowSuccess::Ok)
        })
        .build();

    gst_app::AppSink::builder()
        .caps(&caps)
        .callbacks(callbacks)
        .max_buffers(1)
        .drop(true)
        .sync(true)
        .build()
}

fn make_audio_filter(shared: Arc<SharedMedia>) -> Result<gst::Bin> {
    let bin = gst::Bin::with_name("spectrum-audio-filter");
    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let caps_filter = gst::ElementFactory::make("capsfilter").build()?;

    let audio_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("channels", 2_i32)
        .build();
    caps_filter.set_property("caps", &audio_caps);

    let source_pad = caps_filter
        .static_pad("src")
        .context("capsfilter has no source pad")?;
    source_pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref() else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(caps) = pad.current_caps() else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(structure) = caps.structure(0) else {
            return gst::PadProbeReturn::Ok;
        };
        let (Ok(rate), Ok(channels)) = (
            structure.get::<i32>("rate"),
            structure.get::<i32>("channels"),
        ) else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(pts) = buffer.pts() else {
            return gst::PadProbeReturn::Ok;
        };
        let Ok(map) = buffer.map_readable() else {
            return gst::PadProbeReturn::Ok;
        };
        let channels = channels as usize;
        if channels == 0 {
            return gst::PadProbeReturn::Ok;
        }

        let stream_time = pad
            .sticky_event::<gst::event::Segment>(0)
            .and_then(|event| {
                event
                    .segment()
                    .downcast_ref::<gst::ClockTime>()
                    .map(|segment| segment.to_stream_time(pts))
            })
            .flatten()
            .unwrap_or(pts);
        let frames = map.as_slice().len() / (channels * size_of::<f32>());
        let mut pending = shared.pending_audio.lock().unwrap();
        if buffer.flags().contains(gst::BufferFlags::DISCONT) {
            pending.clear();
            shared.reset_analyser.store(true, Ordering::Release);
        }
        pending.push_back(AudioChunk {
            stream_time_ns: stream_time.nseconds(),
            sample_rate: rate as u32,
            channels,
            frames,
            bytes: map.as_slice().to_vec(),
        });
        while pending.len() > 256 {
            pending.pop_front();
        }
        gst::PadProbeReturn::Ok
    });

    bin.add_many([&convert, &resample, &caps_filter])?;
    gst::Element::link_many([&convert, &resample, &caps_filter])?;

    let sink_pad = convert
        .static_pad("sink")
        .context("audioconvert has no sink pad")?;
    bin.add_pad(
        &gst::GhostPad::builder_with_target(&sink_pad)?
            .name("sink")
            .build(),
    )?;
    bin.add_pad(
        &gst::GhostPad::builder_with_target(&source_pad)?
            .name("src")
            .build(),
    )?;

    Ok(bin)
}

#[cfg(test)]
mod tests {
    use super::{relative_seek_target, uri_from_argument};
    use std::ffi::OsString;

    #[test]
    fn parses_http_media_arguments() {
        let http = "http://media.example/video?id=123";
        let https = "https://media.example/live/playlist.m3u8?token=secret";

        assert_eq!(
            uri_from_argument(OsString::from(http)).unwrap(),
            url::Url::parse(http).unwrap()
        );
        assert_eq!(
            uri_from_argument(OsString::from(https)).unwrap(),
            url::Url::parse(https).unwrap()
        );
    }

    #[test]
    fn converts_local_media_arguments_to_file_uris() {
        let uri = uri_from_argument(OsString::from(".")).unwrap();
        assert_eq!(uri.scheme(), "file");
        assert!(uri.to_file_path().unwrap().is_absolute());
    }

    #[test]
    fn repeated_relative_seeks_accumulate_from_the_pending_target() {
        let second = 1_000_000_000;
        assert_eq!(
            relative_seek_target(10 * second, Some(15 * second), 5, Some(60 * second)),
            20 * second
        );
        assert_eq!(
            relative_seek_target(10 * second, Some(5 * second), -10, Some(60 * second)),
            0
        );
        assert_eq!(
            relative_seek_target(55 * second, None, 10, Some(60 * second)),
            60 * second
        );
    }
}
