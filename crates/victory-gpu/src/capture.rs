//! Presented frames written to PNG files, to see what reaches the screen:
//! with `VICTORY_CAPTURE=<folder>`, every frame whose number is a power of
//! two (the frames the log reports) is saved as `frame-<n>.png`, at half
//! size. Only in builds with diagnostics.

use std::path::PathBuf;

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

/// The folder frames go to, if capturing.
pub(crate) fn folder() -> Option<&'static PathBuf> {
    static FOLDER: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOLDER
        .get_or_init(|| std::env::var_os("VICTORY_CAPTURE").map(PathBuf::from))
        .as_ref()
}

/// Frames listed in `VICTORY_CAPTURE_FRAMES` (comma-separated): saved
/// too, and their draws traced at debug level.
pub(crate) fn listed(n: u64) -> bool {
    static LIST: std::sync::OnceLock<Vec<u64>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        let list = std::env::var("VICTORY_CAPTURE_FRAMES").unwrap_or_default();
        list.split(',')
            .filter_map(|f| f.trim().parse().ok())
            .collect()
    })
    .contains(&n)
        || timed(n)
}

/// combo (local): `VICTORY_CAPTURE_AT_SECS=150,200`: the first frame built after each of
/// those seconds of the run is listed too (menus take a different number of frames each run).
fn timed(n: u64) -> bool {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    static LEFT: std::sync::Mutex<Option<Vec<f64>>> = std::sync::Mutex::new(None);
    static CHOSEN: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
    let start = *START.get_or_init(std::time::Instant::now);
    let mut chosen = CHOSEN.lock().unwrap_or_else(|e| e.into_inner());
    if chosen.contains(&n) {
        return true;
    }
    let mut left = LEFT.lock().unwrap_or_else(|e| e.into_inner());
    let left = left.get_or_insert_with(|| {
        std::env::var("VICTORY_CAPTURE_AT_SECS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .collect()
    });
    let now = start.elapsed().as_secs_f64();
    if let Some(i) = left.iter().position(|&t| now >= t) {
        left.remove(i);
        // (asked first at a present, too late for that frame's targets: the next frame's are
        // noted from its first draw)
        chosen.extend([n, n + 1]);
        tracing::info!(frame = n, secs = now, "combo: frame listed for capture");
        return true;
    }
    false
}

/// A plane selected for this frame, with the same resource ID as its target dump.
pub(crate) fn present_source(
    frame: u64,
    plane: usize,
    composition_index: usize,
    resource: crate::object::This,
    desc: &D3D12_RESOURCE_DESC,
) {
    tracing::info!(
        frame,
        plane,
        composition_index,
        ?resource,
        width = desc.Width,
        height = desc.Height,
        format = desc.Format.0,
        "present source"
    );
}

/// Whether frame `n` is saved.
pub(crate) fn wanted(n: u64) -> bool {
    folder().is_some() && (n.is_power_of_two() || listed(n))
}

/// Saves 8-bit RGBA or BGRA rows (`pitch` bytes apart) as frame `n`.
pub(crate) fn save(n: u64, width: u32, height: u32, pitch: usize, bgra: bool, rows: &[u8]) {
    save_as(&format!("frame-{n}"), width, height, pitch, bgra, rows);
}

/// Saves 8-bit RGBA or BGRA rows as `<name>.png`, at half size.
fn save_as(name: &str, width: u32, height: u32, pitch: usize, bgra: bool, rows: &[u8]) {
    let Some(folder) = folder() else { return };
    let (w, h) = ((width / 2).max(1), (height / 2).max(1));
    // Filter byte 0 per row, then RGB of every other pixel of every other row.
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        let row = &rows[(2 * y).min(height as usize - 1) * pitch..];
        for x in 0..w as usize {
            let p = &row[(2 * x).min(width as usize - 1) * 4..][..3];
            if bgra {
                raw.extend([p[2], p[1], p[0]]);
            } else {
                raw.extend_from_slice(p);
            }
        }
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend(w.to_be_bytes());
    header.extend(h.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut png, b"IHDR", &header);
    chunk(
        &mut png,
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 3),
    );
    chunk(&mut png, b"IEND", &[]);
    let path = folder.join(format!("{name}.png"));
    match std::fs::write(&path, png) {
        Ok(()) => tracing::info!(path = %path.display(), "frame saved"),
        Err(e) => tracing::warn!(error = %e, path = %path.display(), "frame not saved"),
    }
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend((data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend(kind);
    png.extend(data);
    let crc = crc32(&png[start..]);
    png.extend(crc.to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Records a copy of `texture` (in the copy-source state) into a
/// readback buffer.
pub(crate) fn read_back(
    device: &ID3D12Device,
    list: &ID3D12GraphicsCommandList,
    texture: &ID3D12Resource,
) -> Option<(ID3D12Resource, D3D12_PLACED_SUBRESOURCE_FOOTPRINT)> {
    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let mut total = 0;
    // SAFETY: a live texture; outputs for one subresource.
    unsafe {
        let desc = texture.GetDesc();
        device.GetCopyableFootprints(
            &desc,
            0,
            1,
            0,
            Some(&mut footprint),
            None,
            None,
            Some(&mut total),
        );
    }
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_READBACK,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: total,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    // SAFETY: plain creation; an open list; locations naming live resources.
    unsafe {
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut buffer,
            )
            .ok()?;
        let buffer = buffer?;
        let to = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&buffer),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: footprint,
            },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(texture),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
        Some((buffer, footprint))
    }
}

/// Saves a frame read back by [`read_back`] (8-bit color only).
pub(crate) fn save_read_back(
    frame: u64,
    buffer: &ID3D12Resource,
    footprint: &D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
) {
    let f = footprint.Footprint;
    let bgra = match f.Format {
        DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => false,
        DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => true,
        other => {
            tracing::warn!(format = other.0, "frame not saved: not 8-bit color");
            return;
        }
    };
    let mut data = std::ptr::null_mut();
    // SAFETY: the copy finished (every submission is waited for); the
    // mapping spans the footprint's rows.
    unsafe {
        if buffer.Map(0, None, Some(&mut data)).is_err() {
            return;
        }
        let len = f.RowPitch as usize * f.Height as usize;
        let rows = std::slice::from_raw_parts(data.cast::<u8>(), len);
        save(frame, f.Width, f.Height, f.RowPitch as usize, bgra, rows);
        buffer.Unmap(0, None);
    }
}

/// With `VICTORY_DUMP_TARGETS` set: title textures the frames listed in
/// `VICTORY_CAPTURE_FRAMES` render to or write, in first-use order, saved
/// at that frame's present as `frame-<n>-<i>-<resource>-<w>x<h>-f<format>.png`
/// (what each holds at the end of the frame; formats normalized to show
/// structure: see [`Scale`]). Each that is not 8-bit colour is logged first
/// as `target values`, with its largest finite value and how many of its
/// channel values are non-zero.
/// The host copies are held: the title may release a target before the
/// frame is presented.
static TARGETS: std::sync::Mutex<Vec<(usize, std::sync::Arc<crate::resource::HostTexture>)>> =
    std::sync::Mutex::new(Vec::new());

fn dump_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| folder().is_some() && std::env::var_os("VICTORY_DUMP_TARGETS").is_some())
}

/// Notes a title texture the frame being built renders to or writes.
pub(crate) fn note_target(resource: crate::object::This) {
    if resource.is_null() || !dump_on() || !listed(crate::queue::presented() + 1) {
        return;
    }
    let mut targets = TARGETS.lock().unwrap_or_else(|e| e.into_inner());
    if !targets.iter().any(|&(r, _)| r == resource as usize)
        && let Some(copy) = crate::resource::host_copy(resource)
    {
        targets.push((resource as usize, copy));
    }
}

/// At the present of frame `frame`: saves the noted targets.
pub(crate) fn dump_targets(frame: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    let targets = std::mem::take(&mut *TARGETS.lock().unwrap_or_else(|e| e.into_inner()));
    let Some(host) = crate::host::host() else { return };
    for (i, (resource, copy)) in targets.iter().enumerate() {
        let texture = &copy.texture;
        // SAFETY: a live resource.
        let d = unsafe { texture.GetDesc() };
        if d.SampleDesc.Count > 1 || d.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D {
            continue;
        }
        // Every submission has run; the dump's list moves it there and back.
        let state = copy.state.load(Relaxed);
        dump_one(host, texture, state, frame, i, *resource as crate::object::This, &d);
    }
}

fn dump_one(
    host: &crate::host::Host,
    texture: &ID3D12Resource,
    state: u32,
    frame: u64,
    i: usize,
    resource: crate::object::This,
    d: &D3D12_RESOURCE_DESC,
) {
    let Ok(list) = host.new_list() else { return };
    let state = D3D12_RESOURCE_STATES(crate::host::legal_state(texture, state) as i32);
    let copy = D3D12_RESOURCE_STATE_COPY_SOURCE;
    // SAFETY: an open list of ours and a live texture.
    let read = unsafe {
        if state != copy {
            list.list
                .ResourceBarrier(&[crate::host::transition(texture, state, copy)]);
        }
        let read = read_back(&host.device, &list.list, texture);
        if state != copy {
            list.list
                .ResourceBarrier(&[crate::host::transition(texture, copy, state)]);
        }
        read
    };
    list.close();
    host.execute(&[&list]);
    let Some((buffer, footprint)) = read else { return };
    let f = footprint.Footprint;
    let mut data = std::ptr::null_mut();
    // SAFETY: the copy finished; the mapping spans the footprint's rows.
    unsafe {
        if buffer.Map(0, None, Some(&mut data)).is_err() {
            return;
        }
        let pitch = f.RowPitch as usize;
        let rows = std::slice::from_raw_parts(data.cast::<u8>(), pitch * f.Height as usize);
        if let Some(rgba) = to_rgba(f.Format.0 as u32, f.Width as usize, f.Height as usize, pitch, rows) {
            let name = format!(
                "frame-{frame}-{i:02}-{resource:?}-{}x{}-f{}",
                d.Width, d.Height, d.Format.0
            );
            save_as(&name, f.Width, f.Height, f.Width as usize * 4, false, &rgba);
        } else {
            tracing::info!(format = f.Format.0, ?resource, "target not dumped: format");
        }
        buffer.Unmap(0, None);
    }
}

fn half(h: u16) -> f32 {
    let (sign, exp, man) = ((h >> 15) as u32, ((h >> 10) & 0x1f) as i32, (h & 0x3ff) as f32);
    let v = match exp {
        0 => man * 2f32.powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + man / 1024.0) * 2f32.powi(e - 15),
    };
    if sign == 1 { -v } else { v }
}

/// A float packed in `bits` with a 5-bit exponent and `man` mantissa bits
/// (the 11- and 10-bit floats of R11G11B10).
fn small_float(bits: u32, man: u32) -> f32 {
    let (exp, m) = ((bits >> man) & 0x1f, (bits & ((1 << man) - 1)) as f32);
    match exp {
        0 => m / (1 << man) as f32 * 2f32.powi(-14),
        31 => f32::INFINITY,
        e => (1.0 + m / (1 << man) as f32) * 2f32.powi(e as i32 - 15),
    }
}

/// Rows of a host format as 8-bit RGBA: 8-bit color as is, everything else
/// as channel values scaled by [`Scale`] (single channels as grey). `None`
/// for formats not handled.
fn to_rgba(format: u32, w: usize, h: usize, pitch: usize, rows: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(w * h * 4);
    let px = |x: usize, y: usize, n: usize| &rows[y * pitch + x * n..][..n];
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap_or([0; 4]));
    // Channel values as floats, then scaled.
    type Decode = Box<dyn Fn(&[u8]) -> [f32; 3]>;
    let (n, decode): (usize, Decode) = match format {
        27..=32 => {
            for y in 0..h {
                for x in 0..w {
                    let p = px(x, y, 4);
                    out.extend([p[0], p[1], p[2], 255]);
                }
            }
            return Some(out);
        }
        87..=93 => {
            for y in 0..h {
                for x in 0..w {
                    let p = px(x, y, 4);
                    out.extend([p[2], p[1], p[0], 255]);
                }
            }
            return Some(out);
        }
        23..=25 => (4, Box::new(move |b| {
            let v = u32_at(b, 0);
            [(v & 0x3ff) as f32, (v >> 10 & 0x3ff) as f32, (v >> 20 & 0x3ff) as f32]
        })),
        26 => (4, Box::new(move |b| {
            let v = u32_at(b, 0);
            [small_float(v & 0x7ff, 6), small_float(v >> 11 & 0x7ff, 6), small_float(v >> 22, 5)]
        })),
        9..=11 => (8, Box::new(move |b| [half(u16_at(b, 0)), half(u16_at(b, 1)), half(u16_at(b, 2))])),
        33..=34 => (4, Box::new(move |b| [half(u16_at(b, 0)), half(u16_at(b, 1)), 0.0])),
        1..=2 => (16, Box::new(move |b| {
            [0, 1, 2].map(|i| f32::from_bits(u32_at(b, i)))
        })),
        15..=16 => (8, Box::new(move |b| [f32::from_bits(u32_at(b, 0)), f32::from_bits(u32_at(b, 1)), 0.0])),
        35..=38 => (4, Box::new(move |b| [f32::from(u16_at(b, 0)), f32::from(u16_at(b, 1)), 0.0])),
        48..=52 => (2, Box::new(move |b| [f32::from(b[0]), f32::from(b[1]), 0.0])),
        // Single channels (and depth planes): shown as grey.
        39..=43 => (4, Box::new(move |b| [f32::from_bits(u32_at(b, 0)); 3])),
        19..=22 => (8, Box::new(move |b| [f32::from_bits(u32_at(b, 0)); 3])),
        44..=47 => (4, Box::new(move |b| [(u32_at(b, 0) & 0xff_ffff) as f32; 3])),
        53..=54 => (2, Box::new(move |b| [half(u16_at(b, 0)); 3])),
        55..=59 => (2, Box::new(move |b| [f32::from(u16_at(b, 0)); 3])),
        60..=65 => (1, Box::new(move |b| [f32::from(b[0]); 3])),
        _ => return None,
    };
    let values: Vec<[f32; 3]> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| decode(px(x, y, n)))
        .collect();
    let grey = matches!(format, 19..=22 | 39..=47 | 53..=65);
    // Channels the decoder fills; the rest of each [f32; 3] is padding.
    let channels = if grey {
        1
    } else if matches!(format, 15..=16 | 33..=38 | 48..=52) {
        2
    } else {
        3
    };
    let s = Scale::of(&values, channels, grey);
    tracing::info!(
        format,
        low = s.low,
        high = s.high,
        max = s.max,
        nonzero = s.nonzero,
        finite = s.finite,
        mean = s.mean,
        "target values"
    );
    for v in values {
        let c = if grey {
            [s.byte(v[0]); 3]
        } else {
            [0, 1, 2].map(|k| if k < channels { s.byte(v[k]) } else { 0 })
        };
        out.extend([c[0], c[1], c[2], 255]);
    }
    Some(out)
}

/// How a float target's values map to 0..255 in its dump. Scaling by the
/// largest value let one hot pixel (a light source at thousands) turn a lit
/// HDR target black, so colour runs from 0 to about the 99.9th percentile of
/// the channel values, and single channels (`grey`, depth among them) from
/// about the 0.1st to the 99.9th; values outside saturate. Where that range is
/// empty (most of the target 0, or all of it negative), the minimum to the
/// maximum is used instead; a flat target is black at 0 or below and white
/// above. Percentiles come from whole pixels, one per stride at a jittered
/// place in it (so a stride cannot follow a column pattern), at most
/// [`SAMPLES`] values. Counts, maximum and mean cover every pixel's decoded
/// channels (not a decoder's padding; alpha is not decoded). Non-finite
/// values are left out of all of it and dumped black.
#[derive(Debug, PartialEq)]
struct Scale {
    low: f32,
    high: f32,
    /// The largest finite value.
    max: f32,
    /// Finite decoded values that are not 0, and all finite decoded values.
    nonzero: usize,
    finite: usize,
    /// Over the finite decoded values (approximate where they cancel).
    mean: f64,
}

/// Values the percentiles of a [`Scale`] are taken from at most.
const SAMPLES: usize = 1 << 20;

impl Scale {
    /// The scale of `pixels`, whose first `channels` entries are decoded.
    fn of(pixels: &[[f32; 3]], channels: usize, grey: bool) -> Scale {
        let channels = channels.clamp(1, 3);
        let (mut min, mut max, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0f64);
        let (mut nonzero, mut finite) = (0, 0);
        for &v in pixels.iter().flat_map(|p| &p[..channels]) {
            if v.is_finite() {
                (min, max) = (min.min(v), max.max(v));
                sum += f64::from(v);
                finite += 1;
                nonzero += usize::from(v != 0.0);
            }
        }
        if finite == 0 {
            (min, max) = (0.0, 0.0);
        }
        // Whole pixels, so a stride never lands on one channel only, each at
        // a jittered place in its stride, so a stride that divides a row
        // cannot sample one column pattern only.
        let step = (pixels.len() * channels).div_ceil(SAMPLES).max(1);
        let jitter = |i: usize| (i.wrapping_mul(2_654_435_761) >> 7) % step;
        let mut sample: Vec<f32> = (0..pixels.len().div_ceil(step))
            .filter_map(|i| pixels.get(i * step + jitter(i)))
            .flat_map(|p| p[..channels].iter().copied())
            .filter(|v| v.is_finite())
            .take(SAMPLES)
            .collect();
        let mut at = |q: f64| {
            if sample.is_empty() {
                return 0.0;
            }
            let i = ((sample.len() - 1) as f64 * q).round() as usize;
            *sample.select_nth_unstable_by(i, f32::total_cmp).1
        };
        let (mut low, mut high) = (if grey { at(0.001) } else { 0.0 }, at(0.999));
        if high <= low {
            (low, high) = (min, max);
            if !grey && min >= 0.0 {
                low = 0.0;
            }
        }
        Scale {
            low,
            high,
            max,
            nonzero,
            finite,
            mean: if finite == 0 {
                0.0
            } else {
                sum / finite as f64
            },
        }
    }

    /// A channel value as 0..255 (computed in f64, so even the widest
    /// finite range keeps its ends apart; non-finite is 0; with a flat
    /// range, 0 and below are 0 and the rest 255).
    fn byte(&self, v: f32) -> u8 {
        if !v.is_finite() {
            return 0;
        }
        let range = f64::from(self.high) - f64::from(self.low);
        if range <= 0.0 {
            return if v > 0.0 { 255 } else { 0 };
        }
        ((f64::from(v) - f64::from(self.low)) / range * 255.0).clamp(0.0, 255.0) as u8
    }
}

/// Pixels per side of the grid [`probe`] reads.
const GRID: u32 = 4;
/// Bytes between the grid's pixels in the readback buffer (placement
/// alignment).
const SLOT: u64 = 512;

/// Records copies of a grid of pixels of `texture` (8-bit color, in the
/// copy-source state) into a readback buffer, to tell black frames.
pub(crate) fn probe(
    device: &ID3D12Device,
    list: &ID3D12GraphicsCommandList,
    texture: &ID3D12Resource,
) -> Option<ID3D12Resource> {
    // SAFETY: a live resource.
    let d = unsafe { texture.GetDesc() };
    if !matches!(d.Format.0, 27..=29 | 87 | 90 | 91) {
        return None;
    }
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: SLOT * u64::from(GRID * GRID),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    };
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_READBACK,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    // SAFETY: plain creation; an open list; locations naming live resources.
    unsafe {
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut buffer,
            )
            .ok()?;
        let buffer = buffer?;
        for i in 0..GRID * GRID {
            let (x, y) = (
                (2 * (i % GRID) + 1) * d.Width as u32 / (2 * GRID),
                (2 * (i / GRID) + 1) * d.Height / (2 * GRID),
            );
            let to = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&buffer),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: SLOT * u64::from(i),
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: d.Format,
                            Width: 1,
                            Height: 1,
                            Depth: 1,
                            RowPitch: 256,
                        },
                    },
                },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(texture),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            let pixel = D3D12_BOX {
                left: x,
                top: y,
                front: 0,
                right: x + 1,
                bottom: y + 1,
                back: 1,
            };
            list.CopyTextureRegion(&to, 0, 0, 0, &from, Some(&pixel));
        }
        Some(buffer)
    }
}

/// Whether every pixel [`probe`] read is (near) black.
pub(crate) fn black(buffer: &ID3D12Resource) -> bool {
    let mut data = std::ptr::null_mut();
    // SAFETY: the copies finished (every submission is waited for).
    unsafe {
        if buffer.Map(0, None, Some(&mut data)).is_err() {
            return false;
        }
        let bytes =
            std::slice::from_raw_parts(data.cast::<u8>(), (SLOT * u64::from(GRID * GRID)) as usize);
        let level = bytes
            .chunks(SLOT as usize)
            .flat_map(|pixel| pixel[..3].iter().copied())
            .max()
            .unwrap_or(0);
        buffer.Unmap(0, None);
        let mut levels = LEVELS.lock().unwrap_or_else(|e| e.into_inner());
        if levels.len() == 12 {
            levels.pop_front();
        }
        levels.push_back(level);
        level < 8
    }
}

/// The brightest probed channel of the last frames, oldest first: a fade
/// darkens over several, a glitch drops at once.
static LEVELS: std::sync::Mutex<std::collections::VecDeque<u8>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

pub(crate) fn recent_levels() -> Vec<u8> {
    let levels = LEVELS.lock().unwrap_or_else(|e| e.into_inner());
    levels.iter().copied().collect()
}

#[cfg(test)]
mod scale_tests {
    use super::Scale;

    fn colour(values: impl Iterator<Item = f32>) -> Vec<[f32; 3]> {
        values.map(|v| [v, v, v]).collect()
    }

    #[test]
    fn one_hot_pixel_does_not_set_the_scale() {
        // A lit target: 10,000 pixels around 1.0 and one light at 11,088.
        let values = (0..10_000).map(|i| 0.5 + (i % 100) as f32 / 100.0);
        let pixels = colour(values.chain([11_088.0]));
        let s = Scale::of(&pixels, 3, false);
        assert_eq!((s.low, s.max), (0.0, 11_088.0));
        assert!(
            s.high < 1.5,
            "high {} follows the bulk, not the light",
            s.high
        );
        assert_eq!((s.nonzero, s.finite), (30_003, 30_003));
        assert_eq!(s.byte(11_088.0), 255);
        assert!(s.byte(1.0) > 150);
    }

    #[test]
    fn a_strided_sample_keeps_every_channel() {
        // More values than SAMPLES, so the sample strides: a uniform
        // (1, 10, 100) must still scale to its blue channel.
        let pixels = vec![[1.0, 10.0, 100.0]; super::SAMPLES];
        let s = Scale::of(&pixels, 3, false);
        assert_eq!((s.low, s.high), (0.0, 100.0));
        assert_eq!([1.0, 10.0, 100.0].map(|c| s.byte(c)), [2, 25, 255]);
    }

    #[test]
    fn a_stride_dividing_the_row_still_sees_every_column() {
        // Alternating columns of 1 and 100, twice SAMPLES pixels: a stride
        // of 6 pixels, which without jitter would see the 1s only.
        let pixels: Vec<[f32; 3]> = (0..2 * super::SAMPLES)
            .map(|i| [if i % 2 == 0 { 1.0 } else { 100.0 }; 3])
            .collect();
        let s = Scale::of(&pixels, 3, false);
        assert_eq!(s.high, 100.0);
        assert!(s.byte(1.0) < 10);
    }

    #[test]
    fn the_sample_stays_within_its_cap() {
        // 1024 x 1024 with three channels: a stride of 3 pixels.
        let pixels = vec![[1.0f32; 3]; 1 << 20];
        let step = (pixels.len() * 3).div_ceil(super::SAMPLES);
        assert_eq!(step, 3);
        let s = Scale::of(&pixels, 3, false);
        assert_eq!((s.finite, s.high), (3 << 20, 1.0));
    }

    #[test]
    fn padding_channels_are_not_counted() {
        // RG: the decoder fills blue with 0.
        let pixels = vec![[2.0, 4.0, 0.0]; 1000];
        let s = Scale::of(&pixels, 2, false);
        assert_eq!((s.finite, s.nonzero, s.mean), (2000, 2000, 3.0));
        // Grey: one value, repeated in all three.
        let s = Scale::of(&[[5.0; 3]; 10], 1, true);
        assert_eq!((s.finite, s.mean), (10, 5.0));
    }

    #[test]
    fn a_few_lit_pixels_on_black_still_show() {
        // 99.95% zero: the 99.9th percentile is 0, so the maximum is used.
        let values = std::iter::repeat_n(0.0, 19_990).chain(std::iter::repeat_n(7.0, 10));
        let s = Scale::of(&colour(values), 3, false);
        assert_eq!((s.low, s.high, s.nonzero), (0.0, 7.0, 30));
        assert_eq!((s.byte(0.0), s.byte(7.0)), (0, 255));
    }

    #[test]
    fn single_channels_drop_the_outer_tenth_of_a_percent() {
        let values = (0..10_000)
            .map(|i| 0.4 + (i % 1000) as f32 / 5000.0)
            .chain([-1e30, 1e30, f32::NAN, f32::INFINITY]);
        let s = Scale::of(&colour(values), 1, true);
        assert!(s.low >= 0.4 && s.high <= 0.6, "{} .. {}", s.low, s.high);
        assert_eq!((s.max, s.finite), (1e30, 10_002));
        assert_eq!((s.byte(f32::NAN), s.byte(f32::INFINITY)), (0, 0));
    }

    #[test]
    fn negative_and_extreme_ranges_still_map() {
        // All-negative colour: min..max, not 0..(a negative high).
        let s = Scale::of(&colour([-4.0, -2.0].into_iter()), 3, false);
        assert_eq!((s.low, s.high), (-4.0, -2.0));
        assert_eq!((s.byte(-4.0), s.byte(-2.0)), (0, 255));
        // The widest finite range keeps its ends apart (f32 would overflow).
        let s = Scale::of(&colour([-f32::MAX, f32::MAX].into_iter()), 1, true);
        assert_eq!((s.byte(-f32::MAX), s.byte(f32::MAX)), (0, 255));
    }

    #[test]
    fn an_empty_or_flat_target_has_a_range() {
        let s = Scale::of(&[], 3, false);
        assert_eq!((s.low, s.high, s.max, s.finite), (0.0, 0.0, 0.0, 0));
        // A target cleared to 0 is black, not white.
        let s = Scale::of(&[[0.0; 3]; 100], 3, false);
        assert_eq!((s.nonzero, s.byte(0.0)), (0, 0));
        let s = Scale::of(&[[3.0; 3]; 100], 1, true);
        assert_eq!((s.low, s.high, s.mean), (3.0, 3.0, 3.0));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_matches_the_png_check_value() {
        assert_eq!(super::crc32(b"IEND"), 0xae42_6082);
    }
}
