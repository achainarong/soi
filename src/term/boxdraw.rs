//! Line and block characters (U+2500–U+259F), drawn as rectangles instead
//! of with the font, so they fill their cells exactly and join up with
//! their neighbours (fonts often draw them a little too narrow or short).
//! Dashed and diagonal lines are left to the font.

/// A rectangle inside a cell, in pixels from its top left corner, and its
/// opacity (shades are see-through).
pub type Piece = (f64, f64, f64, f64, f64);

/// The arms of each line character from U+2500, as up, right, down, left:
/// 0 none, 1 light, 2 heavy, 3 double; `None` for dashed and diagonal ones.
const ARMS: [Option<[u8; 4]>; 128] = {
    let codes: [&[u8; 4]; 128] = [
        b"0101", b"0202", b"1010", b"2020", b"----", b"----", b"----", b"----", // 2500
        b"----", b"----", b"----", b"----", b"0110", b"0210", b"0120", b"0220", // 2508
        b"0011", b"0012", b"0021", b"0022", b"1100", b"1200", b"2100", b"2200", // 2510
        b"1001", b"1002", b"2001", b"2002", b"1110", b"1210", b"2110", b"1120", // 2518
        b"2120", b"2210", b"1220", b"2220", b"1011", b"1012", b"2011", b"1021", // 2520
        b"2021", b"2012", b"1022", b"2022", b"0111", b"0112", b"0211", b"0212", // 2528
        b"0121", b"0122", b"0221", b"0222", b"1101", b"1102", b"1201", b"1202", // 2530
        b"2101", b"2102", b"2201", b"2202", b"1111", b"1112", b"1211", b"1212", // 2538
        b"2111", b"1121", b"2121", b"2112", b"2211", b"1122", b"1221", b"2212", // 2540
        b"1222", b"2122", b"2221", b"2222", b"----", b"----", b"----", b"----", // 2548
        b"0303", b"3030", b"0310", b"0130", b"0330", b"0013", b"0031", b"0033", // 2550
        b"1300", b"3100", b"3300", b"1003", b"3001", b"3003", b"1310", b"3130", // 2558
        b"3330", b"1013", b"3031", b"3033", b"0313", b"0131", b"0333", b"1303", // 2560
        // Rounded corners (╭╮╯╰) are drawn square.
        b"3101", b"3303", b"1313", b"3131", b"3333", b"0110", b"0011", b"1001", // 2568
        b"1100", b"----", b"----", b"----", b"0001", b"1000", b"0100", b"0010", // 2570
        b"0002", b"2000", b"0200", b"0020", b"0201", b"1020", b"0102", b"2010", // 2578
    ];
    let mut arms = [None; 128];
    let mut i = 0;
    while i < 128 {
        let code = codes[i];
        if code[0] != b'-' {
            arms[i] = Some([
                code[0] - b'0',
                code[1] - b'0',
                code[2] - b'0',
                code[3] - b'0',
            ]);
        }
        i += 1;
    }
    arms
};

/// The pieces that make up `ch` in a `width` × `height` cell, or `None` if
/// the font draws it.
pub fn pieces(ch: char, width: f64, height: f64) -> Option<Vec<Piece>> {
    let code = ch as u32;
    match code {
        0x2500..=0x257f => ARMS[(code - 0x2500) as usize].map(|arms| lines(arms, width, height)),
        0x2580..=0x259f => Some(block(code, width, height)),
        _ => None,
    }
}

fn lines([up, right, down, left]: [u8; 4], width: f64, height: f64) -> Vec<Piece> {
    // Whole pixels, so lines are sharp and the same in every cell.
    let light = (width / 8.0).round().max(1.0);
    let thickness = |weight: u8| if weight == 2 { light * 2.0 } else { light };
    let (cx, cy) = ((width / 2.0).floor(), (height / 2.0).floor());
    // The two lines of a double one are this far either side of the middle.
    let gap = light;
    // How far past the middle a single (or heavy) arm reaches: to the far
    // side of whatever crosses it.
    let reach = |a: u8, b: u8| match a.max(b) {
        0 => 0.0,
        3 => gap + light / 2.0,
        weight => thickness(weight) / 2.0,
    };
    // How far past the middle one line of a double arm reaches: it stops at
    // the near line of an arm on its side, runs on into an arm straight
    // ahead, or else (the outside of a corner) meets the far side's line.
    let double_end = |near: u8, ahead: u8, far: u8| {
        let line = |weight: u8| if weight == 3 { gap } else { 0.0 };
        if near != 0 {
            light / 2.0 - line(near)
        } else if ahead != 0 {
            light / 2.0
        } else {
            light / 2.0 + line(far)
        }
    };
    let mut out = Vec::new();
    // A vertical line centered on `x`, or a horizontal one centered on `y`.
    let mut vertical = |x: f64, size: f64, from: f64, to: f64| {
        out.push(((x - size / 2.0).round(), from, size, to - from, 1.0));
    };
    let mut verticals = Vec::new();
    let mut horizontals = Vec::new();
    for (weight, is_up) in [(up, true), (down, false)] {
        if weight == 0 {
            continue;
        }
        let ahead = if is_up { down } else { up };
        let span = |end: f64| {
            if is_up {
                (0.0, cy + end)
            } else {
                (cy - end, height)
            }
        };
        if weight == 3 {
            for (offset, near, far) in [(-gap, left, right), (gap, right, left)] {
                let (from, to) = span(double_end(near, ahead, far));
                verticals.push((cx + offset, light, from, to));
            }
        } else {
            let (from, to) = span(reach(left, right));
            verticals.push((cx, thickness(weight), from, to));
        }
    }
    for (weight, is_left) in [(left, true), (right, false)] {
        if weight == 0 {
            continue;
        }
        let ahead = if is_left { right } else { left };
        let span = |end: f64| {
            if is_left {
                (0.0, cx + end)
            } else {
                (cx - end, width)
            }
        };
        if weight == 3 {
            for (offset, near, far) in [(-gap, up, down), (gap, down, up)] {
                let (from, to) = span(double_end(near, ahead, far));
                horizontals.push((cy + offset, light, from, to));
            }
        } else {
            let (from, to) = span(reach(up, down));
            horizontals.push((cy, thickness(weight), from, to));
        }
    }
    for (x, size, from, to) in verticals {
        vertical(x, size, from, to);
    }
    for (y, size, from, to) in horizontals {
        out.push((from, (y - size / 2.0).round(), to - from, size, 1.0));
    }
    out
}

fn block(code: u32, width: f64, height: f64) -> Vec<Piece> {
    let (w, h) = (width, height);
    let eighth = |n: u32| n as f64 / 8.0;
    let (hw, hh) = ((w / 2.0).round(), (h / 2.0).round());
    let quadrants = |ul: bool, ur: bool, ll: bool, lr: bool| {
        [
            (ul, (0.0, 0.0, hw, hh, 1.0)),
            (ur, (hw, 0.0, w - hw, hh, 1.0)),
            (ll, (0.0, hh, hw, h - hh, 1.0)),
            (lr, (hw, hh, w - hw, h - hh, 1.0)),
        ]
        .into_iter()
        .filter_map(|(on, piece)| on.then_some(piece))
        .collect()
    };
    match code {
        0x2580 => vec![(0.0, 0.0, w, hh, 1.0)],
        0x2581..=0x2588 => {
            let part = (h * eighth(code - 0x2580)).round();
            vec![(0.0, h - part, w, part, 1.0)]
        }
        0x2589..=0x258f => vec![(0.0, 0.0, (w * eighth(0x2590 - code)).round(), h, 1.0)],
        0x2590 => vec![(hw, 0.0, w - hw, h, 1.0)],
        0x2591..=0x2593 => vec![(0.0, 0.0, w, h, (code - 0x2590) as f64 / 4.0)],
        0x2594 => vec![(0.0, 0.0, w, (h / 8.0).round().max(1.0), 1.0)],
        0x2595 => {
            let part = (w / 8.0).round().max(1.0);
            vec![(w - part, 0.0, part, h, 1.0)]
        }
        0x2596 => quadrants(false, false, true, false),
        0x2597 => quadrants(false, false, false, true),
        0x2598 => quadrants(true, false, false, false),
        0x2599 => quadrants(true, false, true, true),
        0x259a => quadrants(true, false, false, true),
        0x259b => quadrants(true, true, true, false),
        0x259c => quadrants(true, true, false, true),
        0x259d => quadrants(false, true, false, false),
        0x259e => quadrants(false, true, true, false),
        _ => quadrants(false, true, true, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_reach_the_cell_edges_and_meet_in_the_middle() {
        // ─ spans the cell; │ spans it the other way; both 1 px light.
        assert_eq!(
            pieces('─', 8.0, 16.0),
            Some(vec![(0.0, 8.0, 4.0, 1.0, 1.0), (4.0, 8.0, 4.0, 1.0, 1.0)])
        );
        let corner = pieces('┌', 8.0, 16.0).unwrap();
        // Down arm to the bottom edge, right arm to the right edge.
        assert!(corner.iter().any(|p| p.1 + p.3 == 16.0));
        assert!(corner.iter().any(|p| p.0 + p.2 == 8.0));
        assert_eq!(
            pieces('═', 8.0, 16.0).unwrap().len(),
            4,
            "two lines, two arms"
        );
        assert_eq!(pieces('┄', 8.0, 16.0), None, "dashed: the font's");
        assert_eq!(pieces('a', 8.0, 16.0), None);
    }

    #[test]
    fn blocks_fill_their_part_of_the_cell() {
        assert_eq!(
            pieces('█', 8.0, 16.0),
            Some(vec![(0.0, 0.0, 8.0, 16.0, 1.0)])
        );
        assert_eq!(
            pieces('▄', 8.0, 16.0),
            Some(vec![(0.0, 8.0, 8.0, 8.0, 1.0)])
        );
        assert_eq!(
            pieces('▌', 8.0, 16.0),
            Some(vec![(0.0, 0.0, 4.0, 16.0, 1.0)])
        );
        assert_eq!(
            pieces('▒', 8.0, 16.0),
            Some(vec![(0.0, 0.0, 8.0, 16.0, 0.5)])
        );
        assert_eq!(pieces('▚', 8.0, 16.0).unwrap().len(), 2);
    }
}
