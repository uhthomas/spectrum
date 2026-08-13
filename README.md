# Spectrum

Basic implementation of an FFT audio spectrum in browser demonstrating fast and good looking visuals similar to that of pre-rendered videos are possible in real-time and in-browser.

Drag and drop a local media file into the page, or press the 'l' key to use.

## Native player

The Rust player reproduces the same analyser settings, quadratic spectrum mask,
and blurred video composite without a browser or DOM. It uses GStreamer for
native decoding and audio-clock synchronization, and `wgpu` for rendering.

It currently targets Linux and requires the GStreamer development libraries,
`playbin3`, the standard audio/video conversion plugins, and an audio sink.
HTTP(S) playback also requires a compatible GStreamer source plugin such as
`souphttpsrc`. Run it with a local path or HTTP(S) URL, or start it empty and
drop either onto the window:

```sh
cargo run --release -- /path/to/video.mkv
cargo run --release -- 'https://media.example/video.mp4?token=secret'
cargo run --release
```

Controls:

- On-screen controls: play/pause, click or drag to seek, volume, mute,
  spectrum settings, and fullscreen;
  they appear on pointer activity and fade out while playing
- Click the video outside a control to play or pause
- `Space`: play or pause
- `Left` / `Right`: seek backward or forward five seconds
- `F`: toggle borderless fullscreen
- `Escape`: quit

The native Gaussian is evaluated entirely on the GPU at half the logical
display resolution, matching the 40 px CSS blur while keeping the effect
real-time. It uses 16-bit intermediates, sRGB filtering, and duplicated video
edges to avoid gradient banding and dark edge halos.
Exact pixels can still differ from Chromium because GPU video colour conversion
and CSS filter implementations are platform-dependent.

The player supplies native Wayland data-device handling because the current
`winit` Wayland backend does not itself deliver file drag-and-drop events. Set
`SPECTRUM_STATS=1` to print render, decode, and analyser throughput once per
second when diagnosing performance.

The gear button opens the spectrum settings panel. Reset restores the browser
preset:

| Setting | Default | Range |
| --- | ---: | ---: |
| Sensitivity | 1.0x | 0.5–2.0x |
| Smoothing | 0.67 | 0–0.95 |
| Point spacing | 40 px | 20–80 px |
| Blur | 40 px | 0–80 px |
| Brightness | 80% | 40–100% |
| Overlay opacity | 10% | 0–30% |

Audio-only files use the full window as their spectrum viewport over a black
background.

## Support

Works in the latest versions of Chrome and Edge (webkit). Firefox has a [bug](https://bugzilla.mozilla.org/show_bug.cgi?id=1579957)
where `clip-path` does not respect `backdrop-filter` and so does not display
correctly.
