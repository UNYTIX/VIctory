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
/// structure: floats scaled by their largest value, depth by its range).
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

/// With `VICTORY_DUMP_TARGETS` set, buffers too: the title memory ranges
/// the listed frames bind for unordered access (a compute's output that
/// later passes read), in first-binding order. Each is logged as
/// `uav buffer <i>` when first bound: with the draw trace on, after the
/// draw or dispatch that binds it. That is not always the writer: a draw
/// can still be dropped after binding, and later binders of the same range
/// are not logged again. At the frame's present each range is summarized
/// in the log (`uav buffer contents`) and, up to [`RAW_MAX`] bytes, saved
/// as `frame-<n>-buf<i>-<address>-<len>.bin`.
///
/// What is read is the range's host copy at the present: what the frame's
/// shaders wrote, unless the title's CPU wrote the same bytes later in the
/// frame (copy-back takes the title's bytes into the copy). The ranges are
/// those declared for copy-back (`Shadow::gpu_writes`), with the same copy:
/// a buffer a shader reaches only through the resource heap, and not
/// through its tables, is not seen. A frame listed by time is noted only
/// from the moment it is listed; the frame after it is whole.
static BUFFERS: std::sync::Mutex<Vec<NotedBuffer>> = std::sync::Mutex::new(Vec::new());

/// A range a listed frame's shaders write: the frame, its host copy,
/// address, length.
type NotedBuffer = (u64, std::sync::Arc<crate::shadow::Shadow>, u64, u64);

/// Ranges noted per frame at most (the world binds a few hundred).
const NOTED_MAX: usize = 4096;
/// Largest range saved as a file (the world also binds views of its whole
/// heap, about 174 MB, which are only summarized).
const RAW_MAX: u64 = 4 << 20;
/// Bytes read at a time to summarize a range.
const PIECE: usize = 1 << 20;

/// Notes a range of title memory the frame being built binds for unordered
/// access.
pub(crate) fn note_buffer(shadow: &std::sync::Arc<crate::shadow::Shadow>, address: u64, len: u64) {
    let frame = crate::queue::presented() + 1;
    if !dump_on() || !listed(frame) {
        return;
    }
    let mut buffers = BUFFERS.lock().unwrap_or_else(|e| e.into_inner());
    let mut this_frame = buffers.iter().filter(|b| b.0 == frame);
    let i = this_frame.clone().count();
    let noted = this_frame
        .any(|(_, s, a, l)| std::sync::Arc::ptr_eq(s, shadow) && (*a, *l) == (address, len));
    if noted || i >= NOTED_MAX {
        return;
    }
    tracing::info!(i, address = format_args!("{address:#x}"), len, "uav buffer");
    buffers.push((frame, shadow.clone(), address, len));
}

/// At the present of frame `frame`: summarizes the ranges noted for it.
/// Notes for the next frame, which other threads may already be recording,
/// stay; stale ones are dropped.
pub(crate) fn dump_buffers(frame: u64) {
    let noted: Vec<NotedBuffer> = {
        let mut buffers = BUFFERS.lock().unwrap_or_else(|e| e.into_inner());
        let (now, later) = buffers.drain(..).partition(|b| b.0 <= frame);
        *buffers = later;
        now
    };
    let this_frame = noted.into_iter().filter(|b| b.0 == frame);
    for (i, (_, shadow, address, len)) in this_frame.enumerate() {
        let mut sum = Summing::new();
        let mut raw = (len <= RAW_MAX).then(Vec::new);
        let read = shadow.read_pieces(address, len, PIECE, |piece| {
            sum.add(piece);
            if let Some(raw) = &mut raw {
                raw.extend_from_slice(piece);
            }
        });
        if !read {
            tracing::warn!(
                frame,
                i,
                address = format_args!("{address:#x}"),
                "uav buffer not read"
            );
            continue;
        }
        let s = sum.finish();
        tracing::info!(
            frame,
            i,
            address = format_args!("{address:#x}"),
            len = s.bytes,
            dwords = s.dwords,
            zero = s.zero,
            nonfinite = s.nonfinite,
            min = s.min,
            max = s.max,
            mean = s.mean,
            "uav buffer contents{}",
            if s.all_zero { " (all zero)" } else { "" }
        );
        let (Some(raw), Some(folder)) = (raw, folder()) else {
            continue;
        };
        let name = format!("frame-{frame}-buf{i:03}-{address:#x}-{}.bin", raw.len());
        let path = folder.join(name);
        if let Err(e) = std::fs::write(&path, &raw) {
            tracing::warn!(error = %e, path = %path.display(), "buffer not saved");
        }
    }
}

/// A buffer's contents in a few numbers, its dwords read as `f32` (the
/// usual element of the buffers lighting reads).
#[derive(Debug, Default, PartialEq)]
struct Stats {
    bytes: usize,
    /// Every byte is 0, a trailing partial dword included.
    all_zero: bool,
    dwords: usize,
    /// Dwords that are 0.
    zero: usize,
    /// Dwords that are NaN or infinite as floats.
    nonfinite: usize,
    /// Over the finite floats (0 when there are none).
    min: f32,
    max: f32,
    mean: f64,
}

/// [`Stats`] built from pieces of a range, read in order; every piece but
/// the last must be a whole number of dwords.
struct Summing {
    s: Stats,
    finite: usize,
    sum: f64,
    min: f32,
    max: f32,
    nonzero: bool,
}

impl Summing {
    fn new() -> Summing {
        Summing {
            s: Stats::default(),
            finite: 0,
            sum: 0.0,
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            nonzero: false,
        }
    }

    fn add(&mut self, bytes: &[u8]) {
        self.s.bytes += bytes.len();
        self.nonzero |= bytes.iter().any(|&b| b != 0);
        for d in bytes.as_chunks::<4>().0 {
            let bits = u32::from_le_bytes(*d);
            let v = f32::from_bits(bits);
            self.s.dwords += 1;
            self.s.zero += usize::from(bits == 0);
            if v.is_finite() {
                self.finite += 1;
                self.sum += f64::from(v);
                self.min = self.min.min(v);
                self.max = self.max.max(v);
            } else {
                self.s.nonfinite += 1;
            }
        }
    }

    fn finish(self) -> Stats {
        let mut s = self.s;
        s.all_zero = !self.nonzero;
        if self.finite > 0 {
            (s.min, s.max, s.mean) = (self.min, self.max, self.sum / self.finite as f64);
        }
        s
    }
}

impl Stats {
    #[cfg(test)]
    fn of(bytes: &[u8]) -> Stats {
        let mut sum = Summing::new();
        sum.add(bytes);
        sum.finish()
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
/// as channel values scaled so the largest finite one is white (single
/// channels: lowest black, highest white, as grey). `None` for formats not
/// handled.
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
    let finite = values.iter().flatten().copied().filter(|v| v.is_finite());
    let (low, high) = if grey {
        finite.fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(v), h.max(v)))
    } else {
        (0.0, finite.fold(0.0f32, f32::max))
    };
    let range = (high - low).max(1e-20);
    tracing::info!(format, low, high, "target values");
    for v in values {
        let c = v.map(|c| (((c - low) / range).clamp(0.0, 1.0) * 255.0) as u8);
        out.extend([c[0], c[1], c[2], 255]);
    }
    Some(out)
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
mod tests {
    use super::Stats;

    #[test]
    fn crc_matches_the_png_check_value() {
        assert_eq!(super::crc32(b"IEND"), 0xae42_6082);
    }

    fn bytes(dwords: &[u32]) -> Vec<u8> {
        dwords.iter().flat_map(|d| d.to_le_bytes()).collect()
    }

    #[test]
    fn an_empty_buffer_reads_as_all_zero() {
        let s = Stats::of(&[0; 64]);
        assert!(s.all_zero);
        assert_eq!((s.bytes, s.dwords, s.zero, s.nonfinite), (64, 16, 16, 0));
        assert_eq!((s.min, s.max, s.mean), (0.0, 0.0, 0.0));
    }

    #[test]
    fn floats_are_summarized_and_nonfinite_ones_counted_apart() {
        let s = Stats::of(&bytes(&[
            1.5f32.to_bits(),
            (-2.0f32).to_bits(),
            0,
            f32::NAN.to_bits(),
            f32::INFINITY.to_bits(),
            4.5f32.to_bits(),
        ]));
        let expected = Stats {
            bytes: 24,
            all_zero: false,
            dwords: 6,
            zero: 1,
            nonfinite: 2,
            min: -2.0,
            max: 4.5,
            mean: 1.0,
        };
        assert_eq!(s, expected);
    }

    #[test]
    fn trailing_bytes_short_of_a_dword_are_left_out() {
        let mut b = bytes(&[0x3f80_0000]); // 1.0
        b.extend([0xff, 0xff]);
        let s = Stats::of(&b);
        assert_eq!((s.bytes, s.dwords, s.zero, s.max), (6, 1, 0, 1.0));
    }

    #[test]
    fn a_nonzero_byte_past_the_last_dword_is_not_all_zero() {
        // An 8-bit buffer of one byte, 0xff: no whole dword, but not empty.
        let s = Stats::of(&[0xff]);
        assert_eq!((s.bytes, s.dwords, s.zero), (1, 0, 0));
        assert!(!s.all_zero);
        let s = Stats::of(&[0, 0, 0, 0, 0, 0x01]);
        assert_eq!((s.dwords, s.zero), (1, 1));
        assert!(!s.all_zero);
    }

    #[test]
    fn pieces_sum_up_to_the_whole() {
        let words: Vec<u32> = (0..1000u32)
            .map(|i| (i as f32 * 0.5 - 100.0).to_bits())
            .collect();
        let mut all = bytes(&words);
        all.push(7);
        let mut sum = super::Summing::new();
        for piece in all.chunks(64) {
            sum.add(piece);
        }
        assert_eq!(sum.finish(), Stats::of(&all));
    }
}
