//! soi's own terminal engine, including images (Kitty graphics protocol).
//!
//! - `pty`: runs the shell on a pseudo-terminal.
//! - `screen`: the terminal state (grid, scrollback, cursor, modes).
//! - `graphics`: Kitty graphics protocol images.
//! - `widget`: the GTK widget that draws all of it and handles input.

mod area;
mod boxdraw;
mod browse;
mod graphics;
mod keys;
mod links;
mod pty;
mod screen;
mod sixel;
mod widget;

pub use screen::{Finished, Progress};
pub use widget::{Input, TermView, default_font};

use screen::Term;

/// The largest APC sequence accepted; images are sent in several anyway.
const MAX_APC_BYTES: usize = 16 * 1024 * 1024;

/// Feeds program output to the terminal. The `vte` parser ignores APC
/// sequences (`ESC _ … ESC \`), which carry Kitty graphics, so those are
/// cut out here and handed to the terminal directly.
#[derive(Default)]
pub struct Feeder {
    parser: vte::Parser,
    /// Inside an APC sequence: its content so far.
    apc: Option<Vec<u8>>,
    /// Discard an overlong APC through its terminator; never execute a prefix.
    apc_overflow: bool,
    /// The previous chunk ended in ESC (inside or outside an APC).
    held_esc: bool,
}

impl Feeder {
    pub fn feed(&mut self, term: &mut Term, bytes: &[u8]) {
        let mut start = 0;
        let mut i = 0;
        while i < bytes.len() {
            let byte = bytes[i];
            if let Some(apc) = &mut self.apc {
                if self.held_esc {
                    self.held_esc = false;
                    if byte == b'\\' {
                        let data = self.apc.take().unwrap_or_default();
                        if !self.apc_overflow {
                            term.apc(&data);
                        }
                        self.apc_overflow = false;
                        i += 1;
                        start = i;
                        continue;
                    }
                    if !self.apc_overflow && apc.len() < MAX_APC_BYTES {
                        apc.push(0x1b);
                    } else {
                        self.apc_overflow = true;
                        apc.clear();
                    }
                }
                match byte {
                    0x1b => self.held_esc = true,
                    _ if !self.apc_overflow && apc.len() < MAX_APC_BYTES => apc.push(byte),
                    _ => {
                        self.apc_overflow = true;
                        apc.clear();
                    }
                }
                i += 1;
                start = i;
                continue;
            }
            if self.held_esc {
                self.held_esc = false;
                if byte == b'_' {
                    self.apc = Some(Vec::new());
                    i += 1;
                    start = i;
                    continue;
                }
                self.parser.advance(term, &[0x1b]);
            }
            if byte == 0x1b {
                match bytes.get(i + 1) {
                    Some(b'_') => {
                        self.parser.advance(term, &bytes[start..i]);
                        self.apc = Some(Vec::new());
                        i += 2;
                        start = i;
                        continue;
                    }
                    None => {
                        self.parser.advance(term, &bytes[start..i]);
                        self.held_esc = true;
                        i += 1;
                        start = i;
                        continue;
                    }
                    Some(_) => {}
                }
            }
            i += 1;
        }
        if self.apc.is_none() && start < bytes.len() {
            self.parser.advance(term, &bytes[start..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regression_apc_escape_bytes_obey_limit() {
        let mut term = Term::new(20, 3, 10);
        let mut feeder = Feeder {
            apc: Some(vec![b'x'; MAX_APC_BYTES]),
            ..Default::default()
        };
        feeder.feed(&mut term, b"\x1b\x1b\x1b\x1b");
        assert!(feeder.apc.as_ref().unwrap().len() <= MAX_APC_BYTES);
    }

    #[test]
    fn regression_overlong_apc_is_discarded_and_text_recovers() {
        let mut term = Term::new(20, 3, 10);
        let mut data = b"Ga=q,i=1,s=1,v=1,f=24;AAAA".to_vec();
        data.resize(MAX_APC_BYTES, b' ');
        let mut feeder = Feeder {
            apc: Some(data),
            ..Default::default()
        };
        feeder.feed(&mut term, b" ");
        feeder.feed(&mut term, b"\x1b");
        feeder.feed(&mut term, b"\\ok");
        assert!(
            term.take_responses().is_empty(),
            "must not execute the truncated query"
        );
        assert_eq!(term.line(0).unwrap().text(0, 20), "ok");
    }

    #[test]
    fn splits_graphics_from_text_across_chunks() {
        let mut term = Term::new(20, 3, 10);
        let mut feeder = Feeder::default();
        let input = b"ab\x1b_Ga=q,i=1,s=1,v=1,f=24;AAAA\x1b\\cd\x1b[31me";
        // Feed one byte at a time to exercise every split point.
        for byte in input {
            feeder.feed(&mut term, &[*byte]);
        }
        assert_eq!(term.line(0).unwrap().text(0, 20), "abcde");
        assert_eq!(term.take_responses(), b"\x1b_Gi=1;OK\x1b\\");
    }
}

/// Hostile and random input must never crash the engine: output comes
/// from anywhere (files, servers), and a panic would take soi down.
#[cfg(test)]
mod fuzz_tests {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            // xorshift64*
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Bytes biased towards escape sequences and their parameters.
    fn chunk(rng: &mut Rng, len: usize) -> Vec<u8> {
        const PIECES: &[&[u8]] = &[
            b"\x1b[",
            b"\x1b]",
            b"\x1b_G",
            b"\x1bP",
            b"\x1bPq",
            b"\x1bP0;1;0q\"1;1;64;64",
            b"#1;2;100;50;0",
            b"!300~",
            b"\x1b\\",
            b"\x07",
            b"\x1b",
            b"\x9b",
            b";",
            b":",
            b"?",
            b">",
            b"!",
            b" ",
            b"0",
            b"1",
            b"9",
            b"65535",
            b"4294967295",
            b"-1",
            b"99999999999999999999",
            b"m",
            b"H",
            b"J",
            b"K",
            b"L",
            b"M",
            b"P",
            b"@",
            b"r",
            b"h",
            b"l",
            b"b",
            b"S",
            b"T",
            b"X",
            b"q",
            b"t",
            b"n",
            b"c",
            b"\r\n",
            b"\t",
            b"\x08",
            b"\x0e",
            b"\x0f",
            "漢".as_bytes(),
            "e\u{301}".as_bytes(),
            "👩‍👩‍👧".as_bytes(),
            b"8;;https://x.io",
            b"a=T,f=32,s=2,v=2,",
            b"a=T,f=100,t=f,",
            b"a=d,d=A",
            b"m=1;",
            b"o=z,",
            b"i=7,",
            b"AAAA",
            b"?1049h",
            b"?1049l",
            b"?6h",
            b"2;5r",
            b"38;2;1;2;3",
            b"38:2::1:2:3",
            b"48;5;300",
        ];
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            match rng.below(4) {
                0 => out.push(rng.next() as u8),
                _ => out.extend_from_slice(PIECES[rng.below(PIECES.len())]),
            }
        }
        out
    }

    fn run(seed: u64, rounds: usize) {
        let mut rng = Rng(seed);
        let mut term = Term::new(40, 10, 50);
        let mut feeder = Feeder::default();
        for round in 0..rounds {
            let len = 1 + rng.below(512);
            let bytes = chunk(&mut rng, len);
            feeder.feed(&mut term, &bytes);
            term.take_responses();
            if round % 17 == 0 {
                term.resize(1 + rng.below(80), 1 + rng.below(30));
            }
            let (cols, rows) = term.size();
            let abs = term.scrolled_off() - rng.below(60) as i64 + rng.below(rows) as i64;
            let _ = term.link_at(abs, rng.below(cols));
            let _ = term.text_between((abs, rng.below(cols)), (abs + 3, rng.below(cols)));
        }
    }

    #[test]
    fn random_input_never_panics() {
        for seed in 1..=40 {
            run(seed, 300);
        }
    }

    /// A much longer run: `cargo test fuzz_long -- --ignored`.
    #[test]
    #[ignore]
    fn fuzz_long() {
        for seed in 1..=2000 {
            run(seed * 7919, 1000);
        }
    }
}

/// Runs a real program through the engine (no window) and prints what it
/// drew. Needs fastfetch: `cargo test fastfetch -- --ignored --nocapture`.
#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    #[ignore]
    fn fastfetch_logo() {
        let (cols, rows, cell) = (120usize, 40usize, (10.0, 20.0));
        let size = portable_pty::PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: (cols as f64 * cell.0) as u16,
            pixel_height: (rows as f64 * cell.1) as u16,
        };
        let argv = ["fastfetch", "--config", "mokka.jsonc"].map(String::from);
        let env = [("TERM", "xterm-256color"), ("TERM_PROGRAM", "soi")];
        let (mut pty, events) = pty::spawn(&argv, &std::env::temp_dir(), &env, size).unwrap();
        let mut term = Term::new(cols, rows, 1000);
        term.set_cell_px(cell.0, cell.1);
        let mut feeder = Feeder::default();
        while let Ok(pty::Event::Output(bytes)) = events.recv_blocking() {
            feeder.feed(&mut term, &bytes);
            pty.write(&term.take_responses()).unwrap();
        }
        for p in &term.graphics.placements {
            println!(
                "image {}x{} px at line {} col {}, shown {}x{} px = {}x{} cells",
                p.image.width,
                p.image.height,
                p.abs_row,
                p.col,
                p.width_px,
                p.height_px,
                p.cols,
                p.rows
            );
        }
        let first = term.scrolled_off() - term.scrollback_len() as i64;
        for abs in first..term.scrolled_off() + rows as i64 {
            let text = term.line(abs).unwrap().text(0, cols);
            if !text.is_empty() {
                println!("{abs:>3}|{text}");
            }
        }
        assert!(!term.graphics.placements.is_empty(), "no image was placed");
    }
}
