//! The Kitty graphics protocol: images sent in APC sequences
//! (`ESC _ G <control> ; <base64 payload> ESC \`), decoded to pixels and
//! placed at cell positions so they scroll with the text.
//!
//! Supported: transmitting (`a=t`), transmitting and showing (`a=T`),
//! showing a stored image (`a=p`), queries (`a=q`) and deleting (`a=d`);
//! RGB, RGBA and PNG data, sent directly (chunked or not), from a file or a
//! temporary file, optionally zlib-compressed. Caller-named files are never
//! deleted. Animation is not supported.
//!
//! Commands come from program output, which can't be trusted (a file being
//! `cat`-ed, a remote server), so everything is bounded: image sizes,
//! memory, the number of images, and which files may be read.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::rc::Rc;

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// Largest decoded image (about 5000×5000 pixels).
const MAX_IMAGE_BYTES: usize = 100 * 1024 * 1024;
/// Largest width or height in pixels.
const MAX_DIMENSION: u32 = 10_000;
/// Largest payload of one transmission, as base64 (chunks included).
const MAX_PAYLOAD_BYTES: usize = 128 * 1024 * 1024;
/// Largest image file read from disk.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Memory for all images together; the oldest are dropped beyond it.
const MAX_TOTAL_BYTES: usize = 320 * 1024 * 1024;
const MAX_PLACEMENTS: usize = 2048;
const MAX_STORED_IMAGES: usize = 512;
/// Largest placement, in cells.
const MAX_CELLS: u32 = 1000;
/// The only answer for files that can't be used, whatever the reason, so
/// output can't probe which files exist.
const FILE_DENIED: &str = "EBADF:cannot use this file";
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Cairo ARGB32 pixels: premultiplied, native-endian (B, G, R, A).
    pub pixels: Vec<u8>,
    /// Unique per decoded image, for caching the widget's surfaces.
    pub serial: u64,
}

pub struct Placement {
    pub image: Rc<Image>,
    pub image_id: u32,
    /// Absolute line of the top edge (see `Term::line`).
    pub abs_row: i64,
    pub col: usize,
    /// Size on screen in pixels.
    pub width_px: f64,
    pub height_px: f64,
    /// Footprint in cells.
    pub cols: usize,
    pub rows: usize,
    pub z: i32,
    /// Shown on the alternate screen.
    pub alt: bool,
}

/// Where the cursor is when a command arrives.
#[derive(Clone, Copy)]
pub struct PlaceContext {
    pub abs_row: i64,
    pub col: usize,
    pub cell_px: (f64, f64),
    pub alt: bool,
}

#[derive(Default)]
pub struct Outcome {
    /// Answer to send back to the program.
    pub reply: Option<String>,
    /// Cells to move the cursor by (columns, rows) after placing an image.
    pub move_cursor: Option<(usize, usize)>,
}

#[derive(Default)]
pub struct Graphics {
    images: HashMap<u32, Rc<Image>>,
    pub placements: Vec<Placement>,
    /// A chunked transmission (`m=1`) in progress: its control data and the
    /// base64 payload so far.
    pending: Option<(Control, Vec<u8>)>,
    next_serial: u64,
}

#[derive(Clone, Debug)]
struct Control {
    action: u8,
    format: u32,
    medium: u8,
    compressed: bool,
    width: u32,
    height: u32,
    id: u32,
    more: bool,
    quiet: u32,
    cols: u32,
    rows: u32,
    no_cursor_move: bool,
    z: i32,
    delete: u8,
    size: usize,
    offset: usize,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            action: b't',
            format: 32,
            medium: b'd',
            compressed: false,
            width: 0,
            height: 0,
            id: 0,
            more: false,
            quiet: 0,
            cols: 0,
            rows: 0,
            no_cursor_move: false,
            z: 0,
            delete: b'a',
            size: 0,
            offset: 0,
        }
    }
}

/// A control value as `T`; 0 when it is missing, malformed or out of
/// range, rather than wrapped into a valid-looking one.
fn parse_number<T: std::str::FromStr + Default>(value: &[u8]) -> T {
    std::str::from_utf8(value)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_default()
}

fn parse_control(text: &[u8]) -> Control {
    let mut control = Control::default();
    for pair in text.split(|b| *b == b',') {
        let (Some(&key), Some(b'=')) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let value = &pair[2..];
        let number = || parse_number::<i64>(value);
        let letter = value.first().copied().unwrap_or(0);
        match key {
            b'a' => control.action = letter,
            b'f' => control.format = parse_number(value),
            b't' => control.medium = letter,
            b'o' => control.compressed = letter == b'z',
            b's' => control.width = parse_number(value),
            b'v' => control.height = parse_number(value),
            b'i' => control.id = parse_number(value),
            b'm' => control.more = number() == 1,
            b'q' => control.quiet = parse_number(value),
            b'c' => control.cols = parse_number(value),
            b'r' => control.rows = parse_number(value),
            b'C' => control.no_cursor_move = number() == 1,
            b'z' => control.z = parse_number(value),
            b'd' => control.delete = letter,
            b'S' => control.size = number().max(0) as usize,
            b'O' => control.offset = number().max(0) as usize,
            _ => {}
        }
    }
    control
}

impl Graphics {
    /// Handles one command (the APC content after `G`).
    pub fn command(&mut self, data: &[u8], context: PlaceContext) -> Outcome {
        let (control_text, payload) = match data.iter().position(|b| *b == b';') {
            Some(i) => (&data[..i], &data[i + 1..]),
            None => (data, &[][..]),
        };
        let control = parse_control(control_text);

        // Check before copying/decoding, including unchunked transmissions.
        if payload.len() > MAX_PAYLOAD_BYTES {
            self.pending = None;
            return Outcome::default();
        }
        // Continuation chunks only carry `m` (and maybe `q`).
        if let Some((first, mut buffer)) = self.pending.take() {
            if payload.len() > MAX_PAYLOAD_BYTES - buffer.len() {
                // Too big: drop the whole transmission before allocating.
                return Outcome::default();
            }
            buffer.extend_from_slice(payload);
            if control.more {
                self.pending = Some((first, buffer));
                return Outcome::default();
            }
            return self.execute(first, &buffer, context);
        }
        if control.more && matches!(control.action, b't' | b'T' | b'q') {
            self.pending = Some((control, payload.to_vec()));
            return Outcome::default();
        }
        self.execute(control, payload, context)
    }

    fn execute(&mut self, control: Control, payload: &[u8], context: PlaceContext) -> Outcome {
        match control.action {
            b'q' => {
                let result = self.decode(&control, payload).map(|_| ());
                Outcome {
                    reply: reply(&control, result, true),
                    move_cursor: None,
                }
            }
            b't' | b'T' => match self.decode(&control, payload) {
                Ok(image) => {
                    let image = Rc::new(image);
                    if control.id != 0 {
                        self.images.insert(control.id, image.clone());
                    }
                    let move_cursor = (control.action == b'T')
                        .then(|| self.place(image, &control, context))
                        .flatten();
                    self.enforce_limits();
                    Outcome {
                        reply: reply(&control, Ok(()), false),
                        move_cursor,
                    }
                }
                Err(err) => Outcome {
                    reply: reply(&control, Err(err), false),
                    move_cursor: None,
                },
            },
            b'p' => match self.images.get(&control.id).cloned() {
                Some(image) => Outcome {
                    move_cursor: self.place(image, &control, context),
                    reply: reply(&control, Ok(()), false),
                },
                None => Outcome {
                    reply: reply(&control, Err("ENOENT:no such image".into()), false),
                    move_cursor: None,
                },
            },
            b'd' => {
                self.delete(&control, context);
                Outcome::default()
            }
            _ => Outcome::default(),
        }
    }

    /// Shows `image` at the cursor. Returns the cursor movement.
    fn place(
        &mut self,
        image: Rc<Image>,
        control: &Control,
        context: PlaceContext,
    ) -> Option<(usize, usize)> {
        let (cell_w, cell_h) = context.cell_px;
        let (w, h) = (image.width as f64, image.height as f64);
        let (c, r) = (control.cols.min(MAX_CELLS), control.rows.min(MAX_CELLS));
        // With only one of c/r given, the other keeps the aspect ratio.
        let (width_px, height_px) = match (c, r) {
            (0, 0) => (w, h),
            (c, 0) => {
                let width = c as f64 * cell_w;
                (width, width * h / w.max(1.0))
            }
            (0, r) => {
                let height = r as f64 * cell_h;
                (height * w / h.max(1.0), height)
            }
            (c, r) => (c as f64 * cell_w, r as f64 * cell_h),
        };
        let max = MAX_CELLS as usize;
        let cols = ((width_px / cell_w).ceil() as usize).clamp(1, max);
        let rows = ((height_px / cell_h).ceil() as usize).clamp(1, max);
        self.placements.push(Placement {
            image,
            image_id: control.id,
            abs_row: context.abs_row,
            col: context.col,
            width_px,
            height_px,
            cols,
            rows,
            z: control.z,
            alt: context.alt,
        });
        self.enforce_limits();
        (!control.no_cursor_move).then_some((cols, rows))
    }

    /// Shows an image decoded elsewhere (Sixel) at the cursor, at its own
    /// size. Returns its footprint in cells.
    pub fn show_pixels(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
        context: PlaceContext,
    ) -> (usize, usize) {
        self.next_serial += 1;
        let image = Rc::new(Image {
            width,
            height,
            pixels: to_cairo(rgba),
            serial: self.next_serial,
        });
        let control = Control {
            no_cursor_move: false,
            ..Control::default()
        };
        self.place(image, &control, context).unwrap_or((1, 1))
    }

    /// Placements to draw, lowest `z` first.
    pub fn draw_order(&self) -> Vec<&Placement> {
        let mut placements: Vec<&Placement> = self.placements.iter().collect();
        placements.sort_by_key(|p| p.z);
        placements
    }

    /// Drops the oldest placements and images beyond the limits.
    fn enforce_limits(&mut self) {
        if self.placements.len() > MAX_PLACEMENTS {
            let excess = self.placements.len() - MAX_PLACEMENTS;
            self.placements.drain(..excess);
        }
        while self.images.len() > MAX_STORED_IMAGES {
            self.drop_oldest_stored();
        }
        loop {
            let mut seen = std::collections::HashSet::new();
            let total: usize = self
                .images
                .values()
                .chain(self.placements.iter().map(|p| &p.image))
                .filter(|image| seen.insert(image.serial))
                .map(|image| image.pixels.len())
                .sum();
            if total <= MAX_TOTAL_BYTES {
                break;
            }
            let oldest = self
                .images
                .values()
                .chain(self.placements.iter().map(|p| &p.image))
                .map(|image| image.serial)
                .min();
            let Some(oldest) = oldest else { break };
            self.placements.retain(|p| p.image.serial != oldest);
            self.images.retain(|_, image| image.serial != oldest);
        }
    }

    fn drop_oldest_stored(&mut self) {
        if let Some(id) = self
            .images
            .iter()
            .min_by_key(|(_, image)| image.serial)
            .map(|(id, _)| *id)
        {
            self.images.remove(&id);
        }
    }

    fn delete(&mut self, control: &Control, context: PlaceContext) {
        let free = control.delete.is_ascii_uppercase();
        match control.delete.to_ascii_lowercase() {
            b'i' => {
                self.placements.retain(|p| p.image_id != control.id);
                if free {
                    self.images.remove(&control.id);
                }
            }
            b'c' => self.placements.retain(|p| {
                !(p.alt == context.alt
                    && (p.abs_row..p.abs_row + p.rows as i64).contains(&context.abs_row)
                    && (p.col..p.col + p.cols).contains(&context.col))
            }),
            // `a` and everything not supported: all visible placements.
            _ => {
                self.placements.retain(|p| p.alt != context.alt);
                if free {
                    self.images.clear();
                }
            }
        }
    }

    /// Removes placements on the screen whose lines were cleared: those on
    /// the alternate screen, or those at or below `from_abs_row` on the
    /// primary one (`None`: all of them).
    pub fn clear_screen(&mut self, alt: bool, from_abs_row: Option<i64>) {
        self.placements.retain(|p| {
            p.alt != alt || from_abs_row.is_some_and(|row| p.abs_row + (p.rows as i64) <= row)
        });
    }

    /// Drops placements that have scrolled entirely out of the scrollback.
    pub fn forget_above(&mut self, oldest_abs_row: i64) {
        self.placements
            .retain(|p| p.alt || p.abs_row + p.rows as i64 > oldest_abs_row);
    }

    fn decode(&mut self, control: &Control, payload: &[u8]) -> Result<Image, String> {
        let result = self.decode_inner(control, payload);
        if control.medium == b'd' {
            result
        } else {
            result.map_err(|_| FILE_DENIED.into())
        }
    }

    fn decode_inner(&mut self, control: &Control, payload: &[u8]) -> Result<Image, String> {
        let engine = GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
        );
        let payload: Vec<u8> = payload
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        let decoded = engine
            .decode(&payload)
            .map_err(|e| format!("EINVAL:bad base64: {e}"))?;
        let mut data = match control.medium {
            b'd' => decoded,
            b'f' | b't' => read_file(control, &decoded)?,
            _ => return Err("EINVAL:unsupported transmission medium".into()),
        };
        if control.compressed {
            let mut inflated = Vec::new();
            flate2::read::ZlibDecoder::new(&data[..])
                .take(MAX_IMAGE_BYTES as u64)
                .read_to_end(&mut inflated)
                .map_err(|e| format!("EINVAL:bad zlib data: {e}"))?;
            data = inflated;
        }
        let (width, height, rgba) = match control.format {
            100 => decode_png(&data)?,
            24 | 32 => {
                let channels = (control.format / 8) as usize;
                let (w, h) = (control.width, control.height);
                if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
                    return Err("EINVAL:bad image size".into());
                }
                rgba_size(w, h)?;
                let needed = w as usize * h as usize * channels;
                if data.len() < needed {
                    return Err("ENODATA:not enough pixel data".into());
                }
                let rgba = if channels == 4 {
                    data.truncate(needed);
                    data
                } else {
                    data[..needed]
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .flat_map(|p| [p[0], p[1], p[2], 255])
                        .collect()
                };
                (w, h, rgba)
            }
            _ => return Err("EINVAL:unsupported format".into()),
        };
        if rgba.len() > MAX_IMAGE_BYTES {
            return Err("EFBIG:image too large".into());
        }
        self.next_serial += 1;
        Ok(Image {
            width,
            height,
            pixels: to_cairo(&rgba),
            serial: self.next_serial,
        })
    }
}

/// The OK/error answer, unless suppressed with `q`. Kitty only answers
/// commands that carry an image id.
fn reply(control: &Control, result: Result<(), String>, always: bool) -> Option<String> {
    if control.id == 0 && !always {
        return None;
    }
    let message = match result {
        Ok(()) if control.quiet >= 1 => return None,
        Ok(()) => "OK".to_string(),
        Err(_) if control.quiet >= 2 => return None,
        Err(err) => err,
    };
    Some(format!("\x1b_Gi={};{message}\x1b\\", control.id))
}

/// Reads an image file named by program output. Only regular files
/// outside the kernel's pseudo file systems, of limited size, that look
/// like the announced image format; anything else gets `FILE_DENIED`.
fn read_file(control: &Control, path: &[u8]) -> Result<Vec<u8>, String> {
    let denied = || FILE_DENIED.to_string();
    let path = std::str::from_utf8(path).map_err(|_| denied())?;
    // Resolving symlinks and `..` first, so the checks see the real file.
    let path = std::fs::canonicalize(path).map_err(|_| denied())?;
    if crate::platform::current()
        .kernel_filesystems()
        .iter()
        .any(|dir| path.starts_with(dir))
    {
        return Err(denied());
    }
    let file = open_image_file(&path).map_err(|_| denied())?;
    let meta = file.metadata().map_err(|_| denied())?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(denied());
    }
    let mut data = Vec::new();
    file.take(MAX_FILE_BYTES)
        .read_to_end(&mut data)
        .map_err(|_| denied())?;

    if control.offset > 0 || control.size > 0 {
        let start = control.offset.min(data.len());
        let end = if control.size > 0 {
            start.saturating_add(control.size).min(data.len())
        } else {
            data.len()
        };
        data = data[start..end].to_vec();
    }
    let looks_right = control.compressed
        || match control.format {
            100 => data.starts_with(PNG_SIGNATURE),
            _ => true, // raw pixels: the size check in `decode` applies
        };
    if looks_right { Ok(data) } else { Err(denied()) }
}

/// Open a canonical absolute path relative to pinned directory descriptors.
/// Reject symlinks in every component, including ones replaced after
/// canonicalize, and never block while opening a pipe or other special file.
fn open_image_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut file = std::fs::File::open("/")?;
    let mut components = path.components().peekable();
    if components.next() != Some(Component::RootDir) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(std::io::ErrorKind::InvalidInput.into());
        };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let last = components.peek().is_none();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if last { 0 } else { libc::O_DIRECTORY };
        // SAFETY: file owns dirfd, name is NUL-terminated, and no creation
        // flags are used, so openat does not require a mode argument.
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        file = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(file)
}

/// Validate the final RGBA allocation, not only the smaller RGB/grayscale
/// source buffer. Call before allocating or converting attacker-sized pixels.
fn rgba_size(width: u32, height: u32) -> Result<usize, String> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|&size| size <= MAX_IMAGE_BYTES)
        .ok_or_else(|| "EFBIG:image too large".into())
}

fn decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("EINVAL:bad PNG: {e}"))?;
    let info = reader.info();
    if info.width > MAX_DIMENSION || info.height > MAX_DIMENSION {
        return Err("EFBIG:PNG too large".into());
    }
    rgba_size(info.width, info.height)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "EFBIG:PNG too large".to_string())?;
    if size > MAX_IMAGE_BYTES {
        return Err("EFBIG:PNG too large".into());
    }
    let mut buffer = vec![0; size];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|e| format!("EINVAL:bad PNG: {e}"))?;
    buffer.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("EINVAL:unexpanded palette PNG".into()),
    };
    Ok((frame.width, frame.height, rgba))
}

/// Straight RGBA to Cairo's premultiplied native-endian ARGB32.
fn to_cairo(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = p[3] as u16;
            let premultiply = |c: u8| ((c as u16 * a + 127) / 255) as u8;
            let (r, g, b) = (premultiply(p[0]), premultiply(p[1]), premultiply(p[2]));
            let argb = u32::from_be_bytes([p[3], r, g, b]);
            argb.to_ne_bytes()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: PlaceContext = PlaceContext {
        abs_row: 5,
        col: 2,
        cell_px: (10.0, 20.0),
        alt: false,
    };

    #[test]
    fn out_of_range_control_numbers_are_invalid_not_wrapped() {
        let control = parse_control(b"i=-1,s=4294967306,v=10,c=-2,z=4294967296");
        assert_eq!(
            (
                control.id,
                control.width,
                control.height,
                control.cols,
                control.z
            ),
            (0, 0, 10, 0, 0)
        );
    }

    fn b64(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }

    #[test]
    fn rejects_oversized_raw_images_before_pixel_conversion() {
        for format in [24, 32] {
            let command = format!("a=q,f={format},s=10000,v=10000,i=1;AAAA");
            let outcome = Graphics::default().command(command.as_bytes(), CONTEXT);
            assert!(outcome.reply.unwrap().contains("EFBIG"));
        }
        assert_eq!(rgba_size(5120, 5120).unwrap(), MAX_IMAGE_BYTES);
        assert!(rgba_size(5121, 5120).is_err());
        assert!(rgba_size(u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn rejects_oversized_grayscale_png_before_decoding_pixels() {
        // Only a header is needed: final RGBA size must be rejected before
        // attempting to read the (absent) compressed pixels.
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, 10000, 10000);
            encoder.set_color(png::ColorType::Grayscale);
            let mut writer = encoder.write_header().unwrap();
            writer.write_chunk(png::chunk::IDAT, &[]).unwrap();
        }
        let error = decode_png(&data).unwrap_err();
        assert!(error.contains("EFBIG"), "{error}");
    }

    #[test]
    fn regression_repeated_placements_are_bounded() {
        let mut graphics = Graphics::default();
        graphics.command(b"a=t,s=1,v=1,i=1;AQIDBA==", CONTEXT);
        for _ in 0..MAX_PLACEMENTS + 50 {
            graphics.command(b"a=p,i=1,C=1,q=2", CONTEXT);
        }
        assert_eq!(graphics.placements.len(), MAX_PLACEMENTS);
    }

    #[test]
    fn regression_temporary_requests_do_not_delete_files() {
        let path = std::env::temp_dir().join(format!(
            "tty-graphics-protocol-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"not an image").unwrap();
        let mut graphics = Graphics::default();
        let outcome = graphics.command(
            format!(
                "a=T,t=t,f=100,i=9;{}",
                b64(path.as_os_str().as_encoded_bytes())
            )
            .as_bytes(),
            CONTEXT,
        );
        let survived = path.exists();
        let _ = std::fs::remove_file(&path);
        assert!(
            survived,
            "terminal output must not delete a caller-named file"
        );
        assert!(outcome.reply.unwrap().contains(FILE_DENIED));
    }

    #[test]
    fn regression_valid_temporary_images_are_read_without_deletion() {
        let path = std::env::temp_dir().join(format!(
            "tty-graphics-protocol-{}.rgba",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, [1, 2, 3, 255]).unwrap();
        let mut graphics = Graphics::default();
        let outcome = graphics.command(
            format!(
                "a=T,t=t,f=32,s=1,v=1,i=9;{}",
                b64(path.as_os_str().as_bytes())
            )
            .as_bytes(),
            CONTEXT,
        );
        let survived = path.exists();
        let _ = std::fs::remove_file(&path);
        assert_eq!(outcome.reply.as_deref(), Some("\x1b_Gi=9;OK\x1b\\"));
        assert_eq!(graphics.placements.len(), 1);
        assert!(survived);
    }

    #[test]
    fn regression_fifo_requests_return_without_a_writer() {
        let path = std::env::temp_dir().join(format!("soi-fifo-{}", uuid::Uuid::new_v4()));
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is NUL-terminated and the unique fixture has no prior owner.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let fixture = path.clone();
        let worker = std::thread::spawn(move || {
            let command = format!("a=q,t=f,f=100,i=9;{}", b64(fixture.as_os_str().as_bytes()));
            let outcome = Graphics::default().command(command.as_bytes(), CONTEXT);
            let _ = sender.send(outcome.reply);
        });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
        std::fs::remove_file(&path).unwrap();
        let reply = result
            .expect("opening a named pipe must not block")
            .unwrap();
        worker.join().unwrap();
        assert!(reply.contains(FILE_DENIED));
    }

    #[test]
    fn regression_image_open_rejects_replaced_symlink_components() {
        let dir = std::env::temp_dir().join(format!("soi-image-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        std::os::unix::fs::symlink("/etc", dir.join("changed")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("file")).unwrap();
        let directory_link = open_image_file(&dir.join("changed/passwd"));
        let file_link = open_image_file(&dir.join("file"));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            directory_link.is_err(),
            "reject an ancestor changed after canonicalize"
        );
        assert!(
            file_link.is_err(),
            "reject a final component changed after canonicalize"
        );
    }

    #[test]
    fn transmits_and_places_rgba() {
        let mut graphics = Graphics::default();
        let pixels = [255u8, 0, 0, 255].repeat(25 * 30);
        let command = format!("a=T,f=32,s=25,v=30,i=7;{}", b64(&pixels));
        let outcome = graphics.command(command.as_bytes(), CONTEXT);
        assert_eq!(outcome.reply.as_deref(), Some("\x1b_Gi=7;OK\x1b\\"));
        // 25x30 px with 10x20 px cells covers 3 columns and 2 rows.
        assert_eq!(outcome.move_cursor, Some((3, 2)));
        let placement = &graphics.placements[0];
        assert_eq!((placement.abs_row, placement.col), (5, 2));
        let red = u32::from_ne_bytes(placement.image.pixels[..4].try_into().unwrap());
        assert_eq!(red, 0xffff0000);
    }

    #[test]
    fn joins_chunks_and_scales_to_cells() {
        let mut graphics = Graphics::default();
        let encoded = b64(&[0u8, 255, 0].repeat(4));
        let (first, rest) = encoded.split_at(4);
        let outcome = graphics.command(
            format!("a=T,f=24,s=2,v=2,c=4,r=1,m=1;{first}").as_bytes(),
            CONTEXT,
        );
        assert!(outcome.move_cursor.is_none(), "waits for the last chunk");
        let outcome = graphics.command(format!("m=0;{rest}").as_bytes(), CONTEXT);
        assert_eq!(outcome.move_cursor, Some((4, 1)));
        assert_eq!(graphics.placements[0].width_px, 40.0);
    }

    #[test]
    fn decodes_png_and_keeps_aspect_ratio() {
        let mut png_data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_data, 4, 2);
            encoder.set_color(png::ColorType::Rgb);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[9u8; 4 * 2 * 3]).unwrap();
        }
        let mut graphics = Graphics::default();
        let command = format!("a=T,f=100,c=4,q=2;{}", b64(&png_data));
        let outcome = graphics.command(command.as_bytes(), CONTEXT);
        assert!(outcome.reply.is_none());
        let placement = &graphics.placements[0];
        assert_eq!((placement.width_px, placement.height_px), (40.0, 20.0));
    }

    #[test]
    fn answers_queries_and_reports_errors() {
        let mut graphics = Graphics::default();
        let query = format!("a=q,i=31,s=1,v=1,f=24,t=d;{}", b64(&[0, 0, 0]));
        let outcome = graphics.command(query.as_bytes(), CONTEXT);
        assert_eq!(outcome.reply.as_deref(), Some("\x1b_Gi=31;OK\x1b\\"));
        assert!(graphics.placements.is_empty());
        let outcome = graphics.command(b"a=T,f=32,s=9,v=9,i=2;AAAA", CONTEXT);
        assert!(outcome.reply.unwrap().contains("ENODATA"));
    }

    #[test]
    fn refuses_huge_and_hostile_images() {
        let mut graphics = Graphics::default();
        // Width × height that would overflow without the size limit.
        let outcome = graphics.command(b"a=T,f=32,s=4294967295,v=4294967295,i=1;AAAA", CONTEXT);
        assert!(outcome.reply.unwrap().contains("EINVAL"));
        // A placement claiming billions of rows is capped.
        let pixel = b64(&[1, 2, 3, 4]);
        let command = format!("a=T,s=1,v=1,r=4000000000,c=4000000000;{pixel}");
        let outcome = graphics.command(command.as_bytes(), CONTEXT);
        assert_eq!(
            outcome.move_cursor,
            Some((MAX_CELLS as usize, MAX_CELLS as usize))
        );
    }

    #[test]
    fn reads_only_real_image_files() {
        let mut graphics = Graphics::default();
        let try_file = |graphics: &mut Graphics, path: &str, extra: &str| {
            let command = format!("a=T,t=f,f=100,i=9{extra};{}", b64(path.as_bytes()));
            graphics.command(command.as_bytes(), CONTEXT).reply.unwrap()
        };
        let denied = format!("\x1b_Gi=9;{FILE_DENIED}\x1b\\");
        for path in [
            "/dev/zero",
            "/proc/self/environ",
            "/etc/passwd",
            "/nonexistent",
            "/tmp",
        ] {
            assert_eq!(try_file(&mut graphics, path, ""), denied, "{path}");
        }
        assert_eq!(try_file(&mut graphics, "/etc/passwd", ",O=-5,S=-1"), denied);

        let dir = std::env::temp_dir().join(format!("soi-graphics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png_path = dir.join("pixel.png");
        let mut png_data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_data, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[1, 2, 3, 4])
                .unwrap();
        }
        std::fs::write(&png_path, &png_data).unwrap();
        let reply = try_file(&mut graphics, png_path.to_str().unwrap(), "");
        assert_eq!(reply, "\x1b_Gi=9;OK\x1b\\");

        // "Temporary" files outside the temp dir (via `..`) are not deleted.
        let victim = dir.join("tty-graphics-protocol-victim.png");
        std::fs::write(&victim, &png_data).unwrap();
        let sneaky = format!(
            "{}/../{}/tty-graphics-protocol-victim.png",
            dir.display(),
            dir.file_name().unwrap().to_string_lossy()
        );
        let command = format!("a=T,t=t,f=100,i=9;{}", b64(sneaky.as_bytes()));
        graphics.command(command.as_bytes(), CONTEXT);
        assert!(
            victim.exists(),
            "file outside the temp dir itself must survive"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_memory_bounded() {
        let mut graphics = Graphics::default();
        let pixel = b64(&[1, 2, 3, 4]);
        for _ in 0..MAX_PLACEMENTS + 50 {
            graphics.command(format!("a=T,s=1,v=1;{pixel}").as_bytes(), CONTEXT);
        }
        assert_eq!(graphics.placements.len(), MAX_PLACEMENTS);
        for id in 1..=MAX_STORED_IMAGES as u32 + 20 {
            graphics.command(
                format!("a=t,s=1,v=1,i={id},q=2;{pixel}").as_bytes(),
                CONTEXT,
            );
        }
        assert_eq!(graphics.images.len(), MAX_STORED_IMAGES);
    }

    #[test]
    fn deletes_placements() {
        let mut graphics = Graphics::default();
        let pixel = b64(&[1, 2, 3, 4]);
        graphics.command(format!("a=T,s=1,v=1,i=1;{pixel}").as_bytes(), CONTEXT);
        graphics.command(format!("a=T,s=1,v=1,i=2;{pixel}").as_bytes(), CONTEXT);
        graphics.command(b"a=d,d=i,i=1", CONTEXT);
        assert_eq!(graphics.placements.len(), 1);
        graphics.command(b"a=d", CONTEXT);
        assert!(graphics.placements.is_empty());
    }
}
