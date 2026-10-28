//! Sixel images (`ESC P … q <data> ESC \`), the older image format that
//! tools without the Kitty protocol use (e.g. img2sixel, chafa, gnuplot).
//! Decoded as the data arrives, into straight RGBA.
//!
//! Like Kitty images, the data comes from program output, so the canvas is
//! bounded: pixels outside `MAX_WIDTH` × `MAX_HEIGHT` are dropped.

/// Largest image, in pixels each way.
const MAX_WIDTH: usize = 4096;
const MAX_HEIGHT: usize = 4096;
const MAX_COLORS: usize = 1024;

/// VT340 colors, which registers start with until a program defines its own.
const VT340: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (51, 51, 204),
    (204, 36, 36),
    (51, 204, 51),
    (204, 51, 204),
    (51, 204, 204),
    (204, 204, 51),
    (120, 120, 120),
    (69, 69, 69),
    (87, 87, 153),
    (153, 69, 69),
    (87, 153, 87),
    (153, 87, 153),
    (87, 153, 153),
    (153, 153, 87),
    (204, 204, 204),
];

pub struct Decoder {
    /// What unpainted pixels are; `None`: transparent.
    background: Option<(u8, u8, u8)>,
    colors: Vec<(u8, u8, u8)>,
    color: usize,
    x: usize,
    /// Top of the current six-pixel band.
    y: usize,
    /// Pixels painted so far, as `0xAARRGGBB` (0 is transparent), in rows
    /// of `stride`. Allocated ahead, as images tend to grow bit by bit.
    canvas: Vec<u32>,
    stride: usize,
    /// The size used so far.
    width: usize,
    height: usize,
    /// The size the program announced (raster attributes), if any.
    declared: Option<(usize, usize)>,
    /// A command being read (`#`, `!` or `"`) and its numbers so far.
    command: Option<u8>,
    numbers: Vec<u32>,
    /// Ignore surplus parameters, including their digits, without storing them.
    extra_parameters: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Decoder {
    /// A decoder for an image whose unpainted pixels are `background`
    /// (`None`: transparent).
    pub fn new(background: Option<(u8, u8, u8)>) -> Self {
        let mut colors = VT340.to_vec();
        colors.resize(MAX_COLORS, (0, 0, 0));
        Self {
            background,
            colors,
            color: 0,
            x: 0,
            y: 0,
            canvas: Vec::new(),
            stride: 0,
            width: 0,
            height: 0,
            declared: None,
            command: None,
            numbers: Vec::new(),
            extra_parameters: false,
        }
    }

    pub fn put(&mut self, byte: u8) {
        if let Some(command) = self.command {
            match byte {
                b'0'..=b'9' => {
                    if self.extra_parameters {
                        return;
                    }
                    if self.numbers.is_empty() {
                        self.numbers.push(0);
                    }
                    #[expect(clippy::unwrap_used, reason = "pushed just above when empty")]
                    let last = self.numbers.last_mut().unwrap();
                    *last = last.saturating_mul(10).saturating_add((byte - b'0') as u32);
                    return;
                }
                b';' => {
                    if self.numbers.is_empty() {
                        self.numbers.push(0);
                    }
                    if self.numbers.len() < 5 {
                        self.numbers.push(0);
                    } else {
                        self.extra_parameters = true;
                    }
                    return;
                }
                _ => {
                    self.command = None;
                    self.extra_parameters = false;
                    let numbers = std::mem::take(&mut self.numbers);
                    if command == b'!' {
                        // A repeat applies to the sixel right after it.
                        let count = numbers.first().copied().unwrap_or(1).max(1);
                        if let b'?'..=b'~' = byte {
                            self.sixel(byte - b'?', count as usize);
                            return;
                        }
                    } else {
                        self.finish_command(command, &numbers);
                    }
                }
            }
        }
        match byte {
            b'?'..=b'~' => self.sixel(byte - b'?', 1),
            b'#' | b'!' | b'"' => self.command = Some(byte),
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.y = (self.y + 6).min(MAX_HEIGHT);
            }
            _ => {}
        }
    }

    fn finish_command(&mut self, command: u8, numbers: &[u32]) {
        let n = |i: usize| numbers.get(i).copied().unwrap_or(0);
        match command {
            // `#Pc` selects a color; `#Pc;Pu;Px;Py;Pz` also defines it, in
            // HLS (Pu 1) or RGB percent (Pu 2).
            b'#' => {
                let index = n(0) as usize % MAX_COLORS;
                self.color = index;
                if numbers.len() >= 5 {
                    let percent = |v: u32| (v.min(100) * 255 / 100) as u8;
                    self.colors[index] = match n(1) {
                        1 => hls(n(2), n(3), n(4)),
                        _ => (percent(n(2)), percent(n(3)), percent(n(4))),
                    };
                }
            }
            // `"Pan;Pad;Ph;Pv`: aspect ratio, then the size.
            b'"' if numbers.len() >= 4 => {
                let (w, h) = (n(2) as usize, n(3) as usize);
                // Only a limit: memory goes to what's actually painted.
                self.declared = Some((w.min(MAX_WIDTH), h.min(MAX_HEIGHT)));
            }
            _ => {}
        }
    }

    /// Paints one column of six pixels (the low bit on top), `count` times.
    fn sixel(&mut self, bits: u8, count: usize) {
        let count = count.min(MAX_WIDTH.saturating_sub(self.x));
        if bits != 0 && count > 0 && self.y < MAX_HEIGHT {
            let bottom = (self.y + 6).min(MAX_HEIGHT);
            self.grow(self.x + count, bottom);
            let (r, g, b) = self.colors[self.color];
            let pixel = 0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
            for bit in 0..6 {
                let y = self.y + bit;
                if bits & (1 << bit) != 0 && y < bottom {
                    let row = y * self.stride;
                    self.canvas[row + self.x..row + self.x + count].fill(pixel);
                }
            }
        }
        self.x += count;
    }

    /// Makes the canvas at least `width` × `height`.
    fn grow(&mut self, width: usize, height: usize) {
        self.width = self.width.max(width);
        self.height = self.height.max(height);
        let rows = self.canvas.len().checked_div(self.stride).unwrap_or(0);
        if self.width <= self.stride && self.height <= rows {
            return;
        }
        let stride = if self.width > self.stride {
            self.width.max(self.stride * 2).min(MAX_WIDTH)
        } else {
            self.stride
        };
        let new_rows = self.height.max(rows * 2).min(MAX_HEIGHT);
        if stride == self.stride {
            self.canvas.resize(stride * new_rows, 0);
        } else {
            let mut canvas = vec![0; stride * new_rows];
            for y in 0..rows {
                canvas[y * stride..y * stride + self.stride]
                    .copy_from_slice(&self.canvas[y * self.stride..(y + 1) * self.stride]);
            }
            self.canvas = canvas;
        }
        self.stride = stride;
    }

    /// The image as (width, height, straight RGBA), if anything was painted.
    pub fn finish(self) -> Option<(u32, u32, Vec<u8>)> {
        let (mut width, mut height) = (self.width, self.height);
        if let Some((w, h)) = self.declared.filter(|&(w, h)| w > 0 && h > 0) {
            width = width.min(w.max(1));
            height = height.min(h.max(1));
        }
        if width == 0 || height == 0 {
            return None;
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for &pixel in &self.canvas[y * self.stride..y * self.stride + width] {
                match (pixel, self.background) {
                    (0, Some((r, g, b))) => rgba.extend_from_slice(&[r, g, b, 255]),
                    _ => {
                        let [a, r, g, b] = pixel.to_be_bytes();
                        rgba.extend_from_slice(&[r, g, b, a]);
                    }
                }
            }
        }
        Some((width as u32, height as u32, rgba))
    }
}

/// Sixel HLS (hue in degrees, with blue at 0; lightness and saturation in
/// percent) to RGB.
fn hls(hue: u32, lightness: u32, saturation: u32) -> (u8, u8, u8) {
    let l = lightness.min(100) as f64 / 100.0;
    let s = saturation.min(100) as f64 / 100.0;
    // Sixel puts blue at 0°, the usual HSL wheel puts red there.
    let h = ((hue % 360 + 240) % 360) as f64 / 360.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    (channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(data: &str) -> Option<(u32, u32, Vec<u8>)> {
        let mut decoder = Decoder::default();
        for byte in data.bytes() {
            decoder.put(byte);
        }
        decoder.finish()
    }

    fn pixel(image: &(u32, u32, Vec<u8>), x: u32, y: u32) -> [u8; 4] {
        let i = ((y * image.0 + x) * 4) as usize;
        image.2[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn paints_bands_colors_and_repeats() {
        // Color 1 is red; two columns of all six pixels, then a second band
        // with the top pixel only, three times.
        let image = decode("#1;2;100;0;0#1!2~-!3@").unwrap();
        assert_eq!((image.0, image.1), (3, 12));
        assert_eq!(pixel(&image, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 1, 5), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 2, 0), [0, 0, 0, 0], "not painted");
        assert_eq!(pixel(&image, 2, 6), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 0, 7), [0, 0, 0, 0]);
    }

    #[test]
    fn fills_what_isnt_painted_with_the_background_unless_transparent() {
        let mut decoder = Decoder::new(Some((1, 2, 3)));
        for byte in b"#1;2;100;0;0#1@-@" {
            decoder.put(*byte);
        }
        let image = decoder.finish().unwrap();
        assert_eq!(pixel(&image, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 0, 1), [1, 2, 3, 255], "the background");
        let image = decode("#1;2;100;0;0#1@-@").unwrap();
        assert_eq!(pixel(&image, 0, 1), [0, 0, 0, 0], "transparent");
    }

    #[test]
    fn overlays_colors_after_a_carriage_return() {
        let image = decode("#1;2;100;0;0#2;2;0;0;100#1~$#2A").unwrap();
        assert_eq!(pixel(&image, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 0, 1), [0, 0, 255, 255], "bit 1 repainted");
    }

    #[test]
    fn uses_the_declared_size_and_stays_bounded() {
        let image = decode("\"1;1;2;3#0!9~").unwrap();
        assert_eq!((image.0, image.1), (2, 3));
        let image = decode("!99999~").unwrap();
        assert_eq!(image.0 as usize, MAX_WIDTH);
        assert!(decode("").is_none());
        assert!(decode("#5;1;0;50;100").is_none(), "only colors");
    }

    #[test]
    fn command_parameters_and_band_positions_stay_bounded() {
        let mut decoder = Decoder::default();
        decoder.put(b'#');
        for _ in 0..100_000 {
            decoder.put(b';');
        }
        assert!(decoder.numbers.len() <= 5, "only five parameters are used");
        decoder.put(b'~');
        for _ in 0..100_000 {
            decoder.put(b'-');
        }
        assert!(decoder.y <= MAX_HEIGHT);
        let image = decode("#1;2;100;0;0;999999#1~").unwrap();
        assert_eq!(pixel(&image, 0, 0), [255, 0, 0, 255]);
        assert_eq!(hls(u32::MAX, 50, 100), hls(u32::MAX % 360, 50, 100));
    }

    #[test]
    fn huge_hls_hues_wrap_without_overflow() {
        let image = decode("#1;1;4294967295;50;100~").unwrap();
        let (r, g, b) = hls(u32::MAX % 360, 50, 100);
        assert_eq!(pixel(&image, 0, 0), [r, g, b, 255]);
    }

    #[test]
    fn converts_hls() {
        assert_eq!(hls(0, 50, 100), (0, 0, 255), "blue at 0°");
        assert_eq!(hls(120, 50, 100), (255, 0, 0));
        assert_eq!(hls(240, 50, 100), (0, 255, 0));
        assert_eq!(hls(0, 100, 0), (255, 255, 255));
    }
}
