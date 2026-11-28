//! THE ANTILOKI WALLPAPER. Omarchy themes ship their own logo as a wallpaper (`backgrounds/omarchy.png`: the name in a pixel face, in the theme's accent on its background). On
//! antiloki's background that reads as the wrong product, so a theme that ships it is offered `antiloki.svg` instead: the same picture with the right name, drawn here in the
//! same chunky pixel style (an 8-pixel cell on a 3840x2160 canvas, centred) in the theme's own `accent` and `background`. An SVG because it needs no image library, is a few
//! kilobytes, and is sharp at any size. Pure: colours in, text out.
//!
//! The letters are rows of runs, `(rows, "x-len x-len ...")` in cells, so the whole word is a table you can read:
//!   A N T I L O K I, 213 cells wide and 42 tall.
const W: u32 = 213;
const H: u32 = 42;
const ROWS: &[(u32, &str)] = &[
    (3, "7-12 31-16 50-7 65-20 93-15 113-7 145-12 167-7 183-9 198-15"),
    (3, "3-20 31-16 50-7 62-26 93-15 113-7 142-18 167-7 180-9 198-15"),
    (3, "0-7 19-7 31-7 39-10 50-7 62-26 97-7 113-7 140-7 155-7 167-7 180-9 202-7"),
    (3, "0-7 19-7 31-7 39-10 50-7 71-8 97-7 113-7 140-7 155-7 167-7 177-9 202-7"),
    (3, "0-7 19-7 31-7 41-16 71-8 97-7 113-7 140-7 155-7 167-7 177-9 202-7"),
    (3, "0-7 19-7 31-7 41-16 71-8 97-7 113-7 140-7 155-7 167-16 202-7"),
    (6, "0-7 19-7 31-7 43-14 71-8 97-7 113-7 140-7 155-7 167-16 202-7"),
    (3, "0-26 31-7 45-12 71-8 97-7 113-7 140-7 155-7 167-16 202-7"),
    (3, "0-26 31-7 45-12 71-8 97-7 113-7 140-7 155-7 167-7 177-9 202-7"),
    (1, "0-26 31-7 47-10 71-8 97-7 113-7 140-7 155-7 167-7 177-9 202-7"),
    (2, "0-7 19-7 31-7 47-10 71-8 97-7 113-7 132-3 140-7 155-7 167-7 177-9 202-7"),
    (1, "0-7 19-7 31-7 47-10 71-8 97-7 113-7 132-3 140-7 155-7 167-7 180-9 202-7"),
    (2, "0-7 19-7 31-7 47-10 71-8 97-7 113-22 140-7 155-7 167-7 180-9 202-7"),
    (3, "0-7 19-7 31-7 49-8 71-8 93-15 113-22 142-18 167-7 180-9 198-15"),
    (3, "0-7 19-7 31-7 49-8 71-8 93-15 113-22 145-12 167-7 183-9 198-15"),
];

const CELL: u32 = 8;
const CANVAS: (u32, u32) = (3840, 2160);

/// `#rrggbb` as given, else `fallback`: a colour from a downloaded theme is never trusted into markup.
fn hex_or(c: &str, fallback: &'static str) -> String {
    let t = c.trim();
    if t.len() == 7 && t.starts_with('#') && t[1..].chars().all(|ch| ch.is_ascii_hexdigit()) { t.to_ascii_lowercase() } else { fallback.to_string() }
}

/// The wallpaper: `background` fills the canvas, `accent` draws the name.
pub fn svg(background: &str, accent: &str) -> String {
    let (bg, fg) = (hex_or(background, "#1a1b26"), hex_or(accent, "#7aa2f7"));
    let (x0, y0) = ((CANVAS.0 - W * CELL) / 2, (CANVAS.1 - H * CELL) / 2);
    let mut d = String::new(); let mut row = 0u32;
    for (n, runs) in ROWS {
        for r in runs.split_whitespace() {
            let Some((x, len)) = r.split_once('-').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))) else { continue };
            d.push_str(&format!("M{} {}h{}v{}h-{}z", x0 + x * CELL, y0 + row * CELL, len * CELL, n * CELL, len * CELL));
        }
        row += n;
    }
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" shape-rendering=\"crispEdges\"><rect width=\"{w}\" height=\"{h}\" fill=\"{bg}\"/><path fill=\"{fg}\" d=\"{d}\"/></svg>", w = CANVAS.0, h = CANVAS.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_size_it_says_and_every_run_fits_inside_it() {
        assert_eq!(ROWS.iter().map(|(n, _)| n).sum::<u32>(), H, "the row groups add up to the height");
        for (_, runs) in ROWS { for r in runs.split_whitespace() { let (x, l) = r.split_once('-').unwrap(); let (x, l): (u32, u32) = (x.parse().unwrap(), l.parse().unwrap()); assert!(l > 0 && x + l <= W, "{r}"); } }
        assert!(W * CELL <= CANVAS.0 && H * CELL <= CANVAS.1, "it fits the canvas");
    }

    #[test]
    fn the_wallpaper_is_the_themes_own_colours_with_the_name_centred() {
        let s = svg("#1a1b26", "#7AA2F7");
        assert!(s.starts_with("<svg ") && s.ends_with("</svg>") && !s.contains("<script"));
        assert!(s.contains("fill=\"#1a1b26\"") && s.contains("fill=\"#7aa2f7\""), "background and accent, lower-cased: {}", &s[..200]);
        // centred: the first run is the A's top (cell 7 of the first row group), at (3840 - 213*8)/2 + 7*8 across and (2160 - 42*8)/2 down
        assert!(s.contains(&format!("M{} {}h", (3840 - 213 * 8) / 2 + 7 * 8, (2160 - 42 * 8) / 2)), "{}", &s[150..260]);
        assert!(s.len() < 40_000, "a few kilobytes, not a picture: {}", s.len());
    }

    #[test]
    fn a_colour_that_is_not_a_hex_colour_never_reaches_the_markup() {
        let s = svg("red\" onload=\"x", "url(#a)");
        assert!(!s.contains("onload") && !s.contains("url("), "{}", &s[..220]);
        assert!(s.contains("#1a1b26") && s.contains("#7aa2f7"), "falls back to antiloki's own pair");
    }
}

#[cfg(test)]
mod dump {
    #[test]
    #[ignore = "writes the wallpaper to $ANTILOKI_WORDMARK_OUT so a person can look at it"]
    fn dump_for_eyes() { if let Ok(p) = std::env::var("ANTILOKI_WORDMARK_OUT") { std::fs::write(p, super::svg("#1a1b26", "#7aa2f7")).unwrap(); } }
}
