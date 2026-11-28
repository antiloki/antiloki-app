//! A TERMINAL NOBODY LOOKS AT (the usage meters are read by typing into a CLI in one). It must still act like a terminal: Windows' pseudo console asks where the cursor is (`ESC[6n`) and
//! SHOWS NOTHING until it is answered, and it paints with cursor moves, not lines, so a screen is read as drawn. Found 2026-11-27: Cursor's meter never appeared on Windows.
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// The one writer of the terminal, shared by the person's stand-in and the answering reader.
#[derive(Clone)]
pub struct Keys(Arc<Mutex<Box<dyn Write + Send>>>);
impl Write for Keys {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { let mut w = self.0.lock().unwrap_or_else(|p| p.into_inner()); let n = w.write(b)?; w.flush()?; Ok(n) }
    fn flush(&mut self) -> std::io::Result<()> { self.0.lock().unwrap_or_else(|p| p.into_inner()).flush() }
}
/// How many times the cursor's place is asked for in `all` from byte `from` on (a question may be cut between two reads). Pure.
fn asked(all: &[u8], from: usize) -> usize { all[from.saturating_sub(3)..].windows(4).filter(|w| *w == b"\x1b[6n").count() }
/// Read everything the CLI prints into the returned buffer, answering as a terminal would.
pub fn attach(mut reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> (Arc<Mutex<Vec<u8>>>, Keys) {
    let keys = Keys(Arc::new(Mutex::new(writer))); let buf = Arc::new(Mutex::new(Vec::<u8>::new())); let (sink, mut answer) = (buf.clone(), keys.clone());
    std::thread::spawn(move || { let mut chunk = [0u8; 8192]; while let Ok(n) = reader.read(&mut chunk) { if n == 0 { break }
        let times = { let mut all = sink.lock().unwrap_or_else(|p| p.into_inner()); let from = all.len(); all.extend_from_slice(&chunk[..n]); asked(&all, from) };
        for _ in 0..times { let _ = answer.write_all(b"\x1b[1;1R"); } } });
    (buf, keys)
}
/// The screen as a person would see it now.
pub fn drawn(bytes: &[u8], rows: u16, cols: u16) -> String { let n = |x: u16| std::num::NonZeroU16::new(x.max(1)).unwrap_or(std::num::NonZeroU16::MIN); let mut p = vt100::Parser::new(n(rows), n(cols), 0); p.process(bytes); p.screen().contents() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_cursor_question_is_seen_once_even_cut_in_two() {
        assert_eq!(asked(b"\x1b[6n", 0), 1); assert_eq!(asked(b"ab\x1b[6n", 4), 1, "cut between two reads"); assert_eq!(asked(b"\x1b[6nhello", 4), 0, "answered already"); assert_eq!(asked(b"x", 0), 0);
    }
    #[test] fn a_screen_painted_with_cursor_moves_reads_as_lines() {
        let s = drawn(b"\x1b[2;2HUsage\x1b[1CPro\x1b[3;2HIncluded\x1b[5C42% used", 10, 60);
        assert!(s.contains("Usage Pro") && s.lines().any(|l| l.trim_start().starts_with("Included") && l.contains("42% used")), "{s:?}");
    }
    #[cfg(unix)]
    #[test] fn a_program_that_waits_for_the_cursor_answer_gets_it() {
        let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize { rows: 10, cols: 40, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut b = portable_pty::CommandBuilder::new("/bin/sh"); b.arg("-c"); b.arg("stty raw -echo; printf '\\033[6n'; head -c 6 >/dev/null; printf ready");
        let mut child = pair.slave.spawn_command(b).unwrap(); let (buf, _keys) = attach(pair.master.try_clone_reader().unwrap(), pair.master.take_writer().unwrap());
        for _ in 0..50 { if String::from_utf8_lossy(&buf.lock().unwrap()).contains("ready") { break } std::thread::sleep(std::time::Duration::from_millis(100)); }
        let said = String::from_utf8_lossy(&buf.lock().unwrap()).to_string(); let _ = child.kill(); assert!(said.contains("ready"), "{said:?}");
    }
}
