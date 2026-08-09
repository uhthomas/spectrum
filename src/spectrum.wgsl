struct Params {
    output_size: vec2<f32>,
    video_size: vec2<f32>,
    content_origin: vec2<f32>,
    content_size: vec2<f32>,
    blur_size: vec2<f32>,
    step_px: f32,
    scale_factor: f32,
    point_count: u32,
    playback_progress: f32,
    volume: f32,
    ui_flags: u32,
    blur_scale: f32,
    filter_brightness: f32,
    filter_opacity: f32,
    filter_padding: f32,
    settings_a: vec4<f32>,
    settings_b: vec4<f32>,
};

struct Spectrum {
    y: array<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;

@vertex
fn fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

fn inside_content(pixel: vec2<f32>) -> bool {
    let local = pixel - params.content_origin;
    return all(local >= vec2<f32>(0.0)) && all(local < params.content_size);
}

fn video_uv(screen_uv: vec2<f32>) -> vec2<f32> {
    let pixel = screen_uv * params.output_size;
    return (pixel - params.content_origin) / params.content_size;
}

// Horizontal blur pass: sample the original video at quarter resolution.
@group(1) @binding(0) var source_texture: texture_2d<f32>;
@group(1) @binding(1) var source_sampler: sampler;

// A sigma=20 kernel (equivalent to 40 logical pixels at half resolution),
// truncated at three sigma. Adjacent taps are paired using bilinear sampling.
const GAUSSIAN_CENTER: f32 = 0.0199967809;
const GAUSSIAN_OFFSETS: array<f32, 30> = array<f32, 30>(
    1.4990625011, 3.4978125140, 5.4965625542, 7.4953126373,
    9.4940627791, 11.4928129950, 13.4915633008, 15.4903137120,
    17.4890642443, 19.4878149131, 21.4865657342, 23.4853167231,
    25.4840678954, 27.4828192666, 29.4815708524, 31.4803226681,
    33.4790747295, 35.4778270520, 37.4765796511, 39.4753325423,
    41.4740857411, 43.4728392629, 45.4715931233, 47.4703473376,
    49.4691019212, 51.4678568896, 53.4666122581, 55.4653680421,
    57.4641242570, 59.4628809179,
);
const GAUSSIAN_WEIGHTS: array<f32, 30> = array<f32, 30>(
    0.0398688470, 0.0393738958, 0.0384984168, 0.0372680887,
    0.0357183296, 0.0338926034, 0.0318403980, 0.0296150053,
    0.0272712415, 0.0248632429, 0.0224424572, 0.0200559299,
    0.0177449578, 0.0155441473, 0.0134808912, 0.0115752417,
    0.0098401405, 0.0082819441, 0.0069011752, 0.0056934246,
    0.0046503310, 0.0037605720, 0.0030108126, 0.0023865651,
    0.0018729346, 0.0014552302, 0.0011194393, 0.0008525683,
    0.0006428617, 0.0004799165,
);

fn scene_sample(screen_uv: vec2<f32>) -> vec4<f32> {
    if (any(params.video_size <= vec2<f32>(0.0))) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // Extend the outermost video pixels into the blur margin. Sampling black
    // outside the content rectangle creates the dark fringe that CSS avoids.
    let half_texel = vec2<f32>(0.5) / params.video_size;
    let uv = clamp(video_uv(screen_uv), half_texel, vec2<f32>(1.0) - half_texel);
    return textureSample(source_texture, source_sampler, uv);
}

@fragment
fn blur_horizontal(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / params.blur_size;
    var color = scene_sample(uv) * GAUSSIAN_CENTER;
    for (var index: u32 = 0u; index < 30u; index = index + 1u) {
        let offset = vec2<f32>(
            GAUSSIAN_OFFSETS[index] * params.blur_scale / params.blur_size.x,
            0.0,
        );
        color += (scene_sample(uv - offset) + scene_sample(uv + offset))
            * GAUSSIAN_WEIGHTS[index];
    }
    return color;
}

// Vertical blur pass: finish the separable convolution.
@fragment
fn blur_vertical(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / params.blur_size;
    var color = textureSample(source_texture, source_sampler, uv) * GAUSSIAN_CENTER;
    for (var index: u32 = 0u; index < 30u; index = index + 1u) {
        let offset = vec2<f32>(
            0.0,
            GAUSSIAN_OFFSETS[index] * params.blur_scale / params.blur_size.y,
        );
        color += textureSample(
            source_texture,
            source_sampler,
            uv - offset,
        ) * GAUSSIAN_WEIGHTS[index];
        color += textureSample(
            source_texture,
            source_sampler,
            uv + offset,
        ) * GAUSSIAN_WEIGHTS[index];
    }
    return color;
}

// Composite pass.
@group(2) @binding(0) var composite_blur: texture_2d<f32>;
@group(2) @binding(1) var composite_blur_sampler: sampler;
@group(2) @binding(2) var label_texture: texture_2d<f32>;
@group(3) @binding(0) var<storage, read> spectrum: Spectrum;

fn bezier(a: f32, control: f32, b: f32, t: f32) -> f32 {
    let inverse = 1.0 - t;
    return inverse * inverse * a + 2.0 * inverse * t * control + t * t * b;
}

fn curve_y(x: f32) -> f32 {
    if (params.point_count < 2u) {
        return params.content_size.y + 1.0;
    }

    let step = params.step_px;
    if (x <= step * 0.5) {
        let t = clamp(x / (step * 0.5), 0.0, 1.0);
        return bezier(spectrum.y[0], spectrum.y[0],
            0.5 * (spectrum.y[0] + spectrum.y[1]), t);
    }

    var point = u32(floor(x / step + 0.5));
    point = clamp(point, 1u, params.point_count - 2u);
    let start = 0.5 * (spectrum.y[point - 1u] + spectrum.y[point]);
    let end = 0.5 * (spectrum.y[point] + spectrum.y[point + 1u]);
    let t = clamp(x / step - (f32(point) - 0.5), 0.0, 1.0);
    return bezier(start, spectrum.y[point], end, t);
}

fn inside_rect(pixel: vec2<f32>, low: vec2<f32>, high: vec2<f32>) -> bool {
    return all(pixel >= low) && all(pixel <= high);
}

fn play_shape(local: vec2<f32>, playing: bool) -> bool {
    if (playing) {
        return inside_rect(local, vec2<f32>(-7.0, -9.0), vec2<f32>(-2.0, 9.0)) ||
            inside_rect(local, vec2<f32>(2.0, -9.0), vec2<f32>(7.0, 9.0));
    }
    return local.x >= -6.0 && local.x <= 10.0 &&
        abs(local.y) <= 10.0 - (local.x + 6.0) * 0.625;
}

fn speaker_shape(local: vec2<f32>) -> bool {
    return inside_rect(local, vec2<f32>(-8.0, -4.0), vec2<f32>(-3.0, 4.0)) ||
        (local.x >= -3.0 && local.x <= 5.0 &&
         abs(local.y) <= local.x * 0.5 + 5.5);
}

fn mute_mark(local: vec2<f32>) -> bool {
    let mark = local - vec2<f32>(8.0, 0.0);
    return abs(mark.x) <= 5.0 && abs(mark.y) <= 5.0 &&
        (abs(mark.y - mark.x) <= 1.25 || abs(mark.y + mark.x) <= 1.25);
}

fn fullscreen_shape(local: vec2<f32>, fullscreen: bool) -> bool {
    if (fullscreen) {
        return inside_rect(local, vec2<f32>(-9.0, -2.0), vec2<f32>(-2.0, 1.0)) ||
            inside_rect(local, vec2<f32>(-2.0, -9.0), vec2<f32>(1.0, -2.0)) ||
            inside_rect(local, vec2<f32>(2.0, -2.0), vec2<f32>(9.0, 1.0)) ||
            inside_rect(local, vec2<f32>(-1.0, -9.0), vec2<f32>(2.0, -2.0)) ||
            inside_rect(local, vec2<f32>(-9.0, -1.0), vec2<f32>(-2.0, 2.0)) ||
            inside_rect(local, vec2<f32>(-2.0, 2.0), vec2<f32>(1.0, 9.0)) ||
            inside_rect(local, vec2<f32>(2.0, -1.0), vec2<f32>(9.0, 2.0)) ||
            inside_rect(local, vec2<f32>(-1.0, 2.0), vec2<f32>(2.0, 9.0));
    }
    return inside_rect(local, vec2<f32>(-10.0, -10.0), vec2<f32>(-3.0, -7.0)) ||
        inside_rect(local, vec2<f32>(-10.0, -10.0), vec2<f32>(-7.0, -3.0)) ||
        inside_rect(local, vec2<f32>(3.0, -10.0), vec2<f32>(10.0, -7.0)) ||
        inside_rect(local, vec2<f32>(7.0, -10.0), vec2<f32>(10.0, -3.0)) ||
        inside_rect(local, vec2<f32>(-10.0, 7.0), vec2<f32>(-3.0, 10.0)) ||
        inside_rect(local, vec2<f32>(-10.0, 3.0), vec2<f32>(-7.0, 10.0)) ||
        inside_rect(local, vec2<f32>(3.0, 7.0), vec2<f32>(10.0, 10.0)) ||
        inside_rect(local, vec2<f32>(7.0, 3.0), vec2<f32>(10.0, 10.0));
}

fn gear_shape(local: vec2<f32>) -> bool {
    let radius = length(local);
    let ring = radius >= 4.0 && radius <= 8.0;
    let teeth = (abs(local.x) <= 2.0 && abs(local.y) <= 11.0) ||
        (abs(local.y) <= 2.0 && abs(local.x) <= 11.0);
    return (ring || teeth) && radius >= 3.0;
}

fn settings_panel(scene: vec4<f32>, pixel: vec2<f32>, opacity: f32) -> vec4<f32> {
    let scale = params.scale_factor;
    let right = params.output_size.x - 16.0 * scale;
    let left = max(16.0 * scale, right - 320.0 * scale);
    let bottom = params.output_size.y - 84.0 * scale;
    let top = bottom - 310.0 * scale;
    if (!inside_rect(pixel, vec2<f32>(left, top), vec2<f32>(right, bottom))) {
        return scene;
    }

    var color = mix(scene.rgb, vec3<f32>(0.025), 0.94 * opacity);
    let edge = 1.0 * scale;
    if (pixel.x <= left + edge || pixel.x >= right - edge ||
        pixel.y <= top + edge || pixel.y >= bottom - edge) {
        color = mix(color, vec3<f32>(0.28), opacity);
    }

    let reset_low = vec2<f32>(left + 16.0 * scale, top + 274.0 * scale);
    let reset_high = vec2<f32>(left + 96.0 * scale, top + 302.0 * scale);
    if (inside_rect(pixel, reset_low, reset_high)) {
        color = mix(color, vec3<f32>(0.16), opacity);
    }
    let enabled = (params.ui_flags & 32u) != 0u;
    let toggle_low = vec2<f32>(right - 62.0 * scale, top + 39.0 * scale);
    let toggle_high = vec2<f32>(right - 20.0 * scale, top + 57.0 * scale);
    if (inside_rect(pixel, toggle_low, toggle_high)) {
        color = mix(color, select(vec3<f32>(0.25), vec3<f32>(0.72), enabled), opacity);
    }
    let toggle_x = select(right - 52.0 * scale, right - 30.0 * scale, enabled);
    if (length(pixel - vec2<f32>(toggle_x, top + 48.0 * scale)) <= 7.0 * scale) {
        color = mix(color, vec3<f32>(0.96), opacity);
    }

    let values = array<f32, 6>(
        params.settings_a.x,
        params.settings_a.y,
        params.settings_a.z,
        params.settings_a.w,
        params.settings_b.x,
        params.settings_b.y,
    );
    let slider_start = left + 140.0 * scale;
    let slider_end = right - 20.0 * scale;
    for (var index: u32 = 0u; index < 6u; index = index + 1u) {
        let center_y = top + (82.0 + f32(index) * 34.0) * scale;
        let value_x = mix(slider_start, slider_end, clamp(values[index], 0.0, 1.0));
        if (inside_rect(
            pixel,
            vec2<f32>(slider_start, center_y - 2.0 * scale),
            vec2<f32>(slider_end, center_y + 2.0 * scale),
        )) {
            color = mix(color, vec3<f32>(0.30), opacity);
        }
        if (inside_rect(
            pixel,
            vec2<f32>(slider_start, center_y - 2.0 * scale),
            vec2<f32>(value_x, center_y + 2.0 * scale),
        ) || length(pixel - vec2<f32>(value_x, center_y)) <= 6.0 * scale) {
            color = mix(color, vec3<f32>(0.92), opacity);
        }
    }
    let atlas_size = vec2<f32>(textureDimensions(label_texture));
    let label_uv = (pixel - vec2<f32>(left, top)) / atlas_size;
    let label_alpha = textureSample(label_texture, composite_blur_sampler, label_uv).r;
    color = mix(color, vec3<f32>(0.92), label_alpha * opacity);
    return vec4<f32>(color, 1.0);
}

fn controls(scene: vec4<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let scale = params.scale_factor;
    let bottom = params.output_size.y;
    let opacity = f32((params.ui_flags >> 8u) & 255u) / 255.0;
    if (opacity <= 0.0) {
        return scene;
    }
    if ((params.ui_flags & 16u) != 0u) {
        let panel_right = params.output_size.x - 16.0 * scale;
        let panel_left = max(16.0 * scale, panel_right - 320.0 * scale);
        let panel_bottom = params.output_size.y - 84.0 * scale;
        let panel_top = panel_bottom - 310.0 * scale;
        if (inside_rect(
            pixel,
            vec2<f32>(panel_left, panel_top),
            vec2<f32>(panel_right, panel_bottom),
        )) {
            return settings_panel(scene, pixel, opacity);
        }
    }
    let shade = smoothstep(bottom - 96.0 * scale, bottom, pixel.y);
    if (shade <= 0.0) {
        return scene;
    }

    var color = scene.rgb * (1.0 - 0.58 * shade * opacity);
    let enabled = (params.ui_flags & 2u) != 0u;
    let foreground = select(vec3<f32>(0.38), vec3<f32>(0.94), enabled);
    let shadow = vec3<f32>(0.01);
    let center_y = bottom - 36.0 * scale;
    let play_center = vec2<f32>(36.0 * scale, center_y);
    let play_local = (pixel - play_center) / scale;
    let playing = (params.ui_flags & 1u) != 0u;
    if (play_shape(play_local - vec2<f32>(1.5), playing)) {
        color = mix(color, shadow, opacity);
    }
    if (play_shape(play_local, playing)) {
        color = mix(color, foreground, opacity);
    }

    let seek_start = 76.0 * scale;
    let seek_end = max(seek_start + 20.0 * scale, params.output_size.x - 286.0 * scale);
    if (inside_rect(
        pixel,
        vec2<f32>(seek_start, center_y - 3.5 * scale),
        vec2<f32>(seek_end + 1.5 * scale, center_y + 4.0 * scale),
    )) {
        color = mix(color, shadow, opacity);
    }
    if (inside_rect(
        pixel,
        vec2<f32>(seek_start, center_y - 2.0 * scale),
        vec2<f32>(seek_end, center_y + 2.0 * scale),
    )) {
        color = mix(color, vec3<f32>(0.5), opacity);
    }
    let seek_value = seek_start + (seek_end - seek_start) * params.playback_progress;
    if (length(pixel - vec2<f32>(seek_value + 1.5 * scale, center_y + 1.5 * scale)) <= 7.0 * scale) {
        color = mix(color, shadow, opacity);
    }
    if (inside_rect(
        pixel,
        vec2<f32>(seek_start, center_y - 2.0 * scale),
        vec2<f32>(seek_value, center_y + 2.0 * scale),
    ) || length(pixel - vec2<f32>(seek_value, center_y)) <= 6.0 * scale) {
        color = mix(color, foreground, opacity);
    }

    let volume_start = max(params.output_size.x - 220.0 * scale, seek_end + 30.0 * scale);
    let volume_end = max(params.output_size.x - 108.0 * scale, volume_start + 20.0 * scale);
    let speaker = vec2<f32>(volume_start - 18.0 * scale, center_y);
    let speaker_local = (pixel - speaker) / scale;
    let muted = (params.ui_flags & 4u) != 0u;
    if (speaker_shape(speaker_local - vec2<f32>(1.5)) ||
        (muted && mute_mark(speaker_local - vec2<f32>(1.5)))) {
        color = mix(color, shadow, opacity);
    }
    if (speaker_shape(speaker_local) || (muted && mute_mark(speaker_local))) {
        color = mix(color, foreground, opacity);
    }
    if (inside_rect(
        pixel,
        vec2<f32>(volume_start, center_y - 3.5 * scale),
        vec2<f32>(volume_end + 1.5 * scale, center_y + 4.0 * scale),
    )) {
        color = mix(color, shadow, opacity);
    }
    if (inside_rect(
        pixel,
        vec2<f32>(volume_start, center_y - 2.0 * scale),
        vec2<f32>(volume_end, center_y + 2.0 * scale),
    )) {
        color = mix(color, vec3<f32>(0.5), opacity);
    }
    let volume_value = volume_start + (volume_end - volume_start) * params.volume;
    if (length(pixel - vec2<f32>(volume_value + 1.5 * scale, center_y + 1.5 * scale)) <= 7.0 * scale) {
        color = mix(color, shadow, opacity);
    }
    if (inside_rect(
        pixel,
        vec2<f32>(volume_start, center_y - 2.0 * scale),
        vec2<f32>(volume_value, center_y + 2.0 * scale),
    ) || length(pixel - vec2<f32>(volume_value, center_y)) <= 6.0 * scale) {
        color = mix(color, foreground, opacity);
    }

    let gear_center = vec2<f32>(params.output_size.x - 68.0 * scale, center_y);
    let gear_local = (pixel - gear_center) / scale;
    if (gear_shape(gear_local - vec2<f32>(1.5))) {
        color = mix(color, shadow, opacity);
    }
    if (gear_shape(gear_local)) {
        color = mix(color, foreground, opacity);
    }

    let fullscreen_center = vec2<f32>(params.output_size.x - 28.0 * scale, center_y);
    let fullscreen_local = (pixel - fullscreen_center) / scale;
    let is_fullscreen = (params.ui_flags & 8u) != 0u;
    if (fullscreen_shape(fullscreen_local - vec2<f32>(1.5), is_fullscreen)) {
        color = mix(color, shadow, opacity);
    }
    if (fullscreen_shape(fullscreen_local, is_fullscreen)) {
        color = mix(color, foreground, opacity);
    }
    return vec4<f32>(color, 1.0);
}

fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
    let low = value / 12.92;
    let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, value <= vec3<f32>(0.04045));
}

@fragment
fn composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let screen_uv = position.xy / params.output_size;
    if (!inside_content(position.xy)) {
        return controls(vec4<f32>(0.0, 0.0, 0.0, 1.0), position.xy);
    }

    var raw = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    if (all(params.video_size > vec2<f32>(0.0))) {
        let raw_srgb = textureSample(source_texture, source_sampler, video_uv(screen_uv));
        raw = vec4<f32>(srgb_to_linear(raw_srgb.rgb), raw_srgb.a);
    }
    let local = position.xy - params.content_origin;
    if (params.point_count >= 2u && local.y >= curve_y(local.x)) {
        let blurred = textureSample(composite_blur, composite_blur_sampler, screen_uv);
        // CSS filter functions, including blur and brightness, operate in
        // sRGB. The translucent white background is composited afterwards.
        let filtered_srgb = clamp(
            mix(
                blurred.rgb * params.filter_brightness,
                vec3<f32>(1.0),
                params.filter_opacity,
            ),
            vec3<f32>(0.0),
            vec3<f32>(1.0),
        );
        return controls(
            vec4<f32>(srgb_to_linear(filtered_srgb), raw.a),
            position.xy,
        );
    }
    return controls(raw, position.xy);
}
