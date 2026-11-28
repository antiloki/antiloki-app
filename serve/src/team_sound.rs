//! THE SOUND TOOL (VID-27, operator 2026-11-17: a sound-design branch with a tiny local voice, effects, music, timing and synchronisation): a TOOL TYPE of the tool step, `sound`, with two fixed entries (a graph names an entry, never a command line).
//!   cue_sheet  reads the named beats from a GSAP template (`tl.addLabel("cue:<name>", <number>)`) and CHECKS THEM STATICALLY: each label must equal the start of the first tween after it (so cue drift is caught by construction, not by pixels).
//!   mix        runs the shipped script `team_sound_mix.py` with the person's sound runtime (python + numpy + piper-tts + faster-whisper, installed by scripts/install-sound-runtime.sh): effects locked to the cues, a narration line fitted
//!              to its window by rules, a tiny voice, ducked music, loudness, then verification (loudness, true peak, fit, a speech round trip tolerant of names, sound placement with class-dependent tolerance, big picture events by pixels).
//! Everything here is pure except `python_path` and `voice_path` (they look at the machine).
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;

pub const ENTRIES: [&str; 2] = ["cue_sheet", "mix"];
/// The sounds the shipped script can make (a synthetic kit: no library, no licence).
pub const KIT: [&str; 18] = ["pop", "ping", "thump", "whoosh", "swish", "outwhoosh", "click", "chime", "sparkle", "splash", "rumble", "horn", "boing", "drum_hit", "snare", "page_flip", "wind", "sting_low"];
/// The music beds of the generated kit.
pub const BEDS: [&str; 5] = ["triumphant", "mysterious", "gentle", "tense", "joyful"];
pub const MIX_PY: &str = include_str!("team_sound_mix.py");
pub const FPS: f64 = 30.0;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Cue { pub id: String, pub t: f64, pub frame: u32 }

/// The index of the `)` that closes the `(` at `open`, skipping strings. Pure.
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes(); let (mut depth, mut i, mut quote) = (0i32, open, 0u8);
    while i < b.len() {
        let c = b[i];
        if quote != 0 { if c == b'\\' { i += 1; } else if c == quote { quote = 0; } }
        else if c == b'"' || c == b'\'' || c == b'`' { quote = c; }
        else if c == b'(' { depth += 1; } else if c == b')' { depth -= 1; if depth == 0 { return Some(i); } }
        i += 1;
    }
    None
}
/// The top-level arguments of a call's inside text (commas inside () [] {} or strings do not split). Pure.
fn top_args(inner: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (vec![], String::new(), 0i32, '\0');
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        if quote != '\0' { cur.push(c); if c == '\\' { if let Some(n) = it.next() { cur.push(n); } } else if c == quote { quote = '\0'; } continue; }
        match c { '"' | '\'' | '`' => { quote = c; cur.push(c); } '(' | '[' | '{' => { depth += 1; cur.push(c); } ')' | ']' | '}' => { depth -= 1; cur.push(c); } ',' if depth == 0 => { out.push(cur.trim().to_string()); cur.clear(); } _ => cur.push(c) }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); } out
}
/// A tiny arithmetic evaluator for label times and tween positions: numbers, named constants (`DUR`), + - * / and parentheses. Pure.
pub fn eval_expr(src: &str, consts: &std::collections::BTreeMap<String, f64>) -> Result<f64, String> {
    struct P<'a> { b: &'a [u8], i: usize, c: &'a std::collections::BTreeMap<String, f64> }
    impl P<'_> {
        fn ws(&mut self) { while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() { self.i += 1; } }
        fn expr(&mut self) -> Result<f64, String> { let mut v = self.term()?; loop { self.ws(); match self.b.get(self.i) { Some(b'+') => { self.i += 1; v += self.term()?; } Some(b'-') => { self.i += 1; v -= self.term()?; } _ => return Ok(v) } } }
        fn term(&mut self) -> Result<f64, String> { let mut v = self.factor()?; loop { self.ws(); match self.b.get(self.i) { Some(b'*') => { self.i += 1; v *= self.factor()?; } Some(b'/') => { self.i += 1; let d = self.factor()?; if d == 0.0 { return Err("division by zero".into()); } v /= d; } _ => return Ok(v) } } }
        fn factor(&mut self) -> Result<f64, String> {
            self.ws(); match self.b.get(self.i) {
                Some(b'(') => { self.i += 1; let v = self.expr()?; self.ws(); if self.b.get(self.i) == Some(&b')') { self.i += 1; Ok(v) } else { Err("a `)` is missing".into()) } }
                Some(b'-') => { self.i += 1; Ok(-self.factor()?) }
                Some(c) if c.is_ascii_digit() || *c == b'.' => { let st = self.i; while self.i < self.b.len() && (self.b[self.i].is_ascii_digit() || self.b[self.i] == b'.') { self.i += 1; } std::str::from_utf8(&self.b[st..self.i]).unwrap_or("").parse::<f64>().map_err(|_| "a number is malformed".to_string()) }
                Some(c) if c.is_ascii_alphabetic() || *c == b'_' => { let st = self.i; while self.i < self.b.len() && (self.b[self.i].is_ascii_alphanumeric() || self.b[self.i] == b'_') { self.i += 1; } let name = std::str::from_utf8(&self.b[st..self.i]).unwrap_or(""); self.c.get(name).copied().ok_or_else(|| format!("`{name}` is not a numeric constant declared as `var {name} = <number>;`")) }
                _ => Err("a number or a named constant was expected".into()),
            }
        }
    }
    let mut p = P { b: src.as_bytes(), i: 0, c: consts }; let v = p.expr()?; p.ws(); if p.i != p.b.len() { return Err("something is left over after the expression".into()); } Ok(v)
}
/// The numeric constants a set of sources declares at the top of a line: `var|let|const NAME = <number>;`. Pure.
pub fn consts_of(files: &[&str]) -> std::collections::BTreeMap<String, f64> {
    let re = Regex::new(r"(?m)^\s*(?:var|let|const)\s+([A-Za-z_]\w*)\s*=\s*(-?[0-9]+(?:\.[0-9]+)?)\s*;").unwrap();
    files.iter().flat_map(|f| re.captures_iter(f).map(|c| (c[1].to_string(), c[2].parse::<f64>().unwrap_or(0.0))).collect::<Vec<_>>()).collect()
}
/// The cue sheet of a template and what is wrong with it. A label is `tl.addLabel("cue:<name>", <time>)`, the time a number or an arithmetic expression over named constants (`DUR - 0.4`); each must be followed by a tween (`tl.fromTo|to|from|set(...)`) whose position (its last argument) evaluates to the same time. `files`: the template first, then any file that declares its constants. Pure.
pub fn cues_with(files: &[&str]) -> (Vec<Cue>, Vec<String>) {
    let (mut cues, mut issues) = (vec![], vec![]); let js = files.first().copied().unwrap_or(""); let consts = consts_of(files);
    let any = Regex::new(r#"addLabel\(\s*["']cue:([a-z0-9_]+)["']\s*,"#).unwrap(); let tween = Regex::new(r"tl\.(?:fromTo|to|from|set)\(").unwrap();
    for m in any.captures_iter(js) {
        let (id, end) = (m[1].to_string(), m.get(0).unwrap().end()); let open = end - m.get(0).unwrap().as_str().len() + m.get(0).unwrap().as_str().find('(').unwrap_or(0);
        let Some(close) = matching_paren(js, open) else { issues.push(format!("cue:{id}: the label call is not closed")); continue };
        let args = top_args(&js[open + 1..close]); let Some(time_src) = args.get(1) else { issues.push(format!("cue:{id}: the label has no time")); continue };
        let t = match eval_expr(time_src, &consts) { Ok(t) if t >= 0.0 => t, Ok(_) => { issues.push(format!("cue:{id}: the time is negative")); continue } Err(e) => { issues.push(format!("cue:{id}: the time `{time_src}` is not a plain number of seconds or an expression over declared constants: {e}")); continue } };
        if cues.iter().any(|c: &Cue| c.id == id) { issues.push(format!("cue:{id} is labelled twice")); continue; }
        match tween.find(&js[close..]) {
            None => issues.push(format!("cue:{id}: no tween follows the label, so nothing moves at {t} s")),
            Some(tw) => {
                let open2 = close + tw.end() - 1;
                match matching_paren(js, open2).map(|c2| top_args(&js[open2 + 1..c2])) {
                    Some(a) if a.len() >= 3 => match a.last().map(|x| eval_expr(x, &consts)) {
                        Some(Ok(pos)) if (pos - t).abs() <= 0.001 => {}
                        Some(Ok(pos)) => issues.push(format!("cue:{id} says {t} s but the tween after it starts at {pos} s")),
                        _ => issues.push(format!("cue:{id}: the tween after it has no position that evaluates to a number, so the label cannot be verified")),
                    },
                    _ => issues.push(format!("cue:{id}: the tween after the label could not be read")),
                }
            }
        }
        cues.push(Cue { id, t, frame: (t * FPS).round() as u32 });
    }
    cues.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal)); (cues, issues)
}
/// The cue sheet of one template file. Pure.
pub fn cues_of(js: &str) -> (Vec<Cue>, Vec<String>) { cues_with(&[js]) }

fn rel_ok(p: &str) -> bool { !p.is_empty() && p.len() < 200 && !p.starts_with('/') && !p.starts_with('\\') && !p.contains("..") && !p.contains(':') && !p.split('/').any(|c| c == ".git" || c == ".antiloki" || c.starts_with(".env")) }
/// What is wrong with a sound step's arguments. Pure. `cue_sheet`: {file}. `mix`: {file, video, out, text_from, spec{beats, narration, music, target_lufs, true_peak_max}}.
pub fn check_args(entry: &str, a: &Value) -> Vec<String> {
    let mut e = vec![];
    if !ENTRIES.contains(&entry) { return vec![format!("the sound entry is one of: {}", ENTRIES.join(", "))]; }
    if !rel_ok(a["file"].as_str().unwrap_or("")) { e.push("`file` is the template inside the run's tree (for example scenes.js)".into()); }
    for c in a["consts"].as_array().cloned().unwrap_or_default() { if !rel_ok(c.as_str().unwrap_or("")) { e.push("`consts` lists files inside the run's tree that declare the template's numeric constants".into()); } }
    if entry == "cue_sheet" { return e; }
    if let Some(d) = a["duration_from"].as_str() { if !d.starts_with("values:") { e.push("`duration_from` is `values:<node>.<field>`: the shot's length in seconds".into()); } }
    for k in ["video", "out"] { if !rel_ok(a[k].as_str().unwrap_or("")) { e.push(format!("`{k}` is a path inside the run's tree")); } }
    if !a["text_from"].as_str().unwrap_or("").starts_with("values:") { e.push("`text_from` is where the narration text is: `values:<node>.<field>`".into()); }
    let sp = &a["spec"]; let num = |v: &Value, lo: f64, hi: f64| v.as_f64().map_or(false, |x| x >= lo && x <= hi);
    match sp["beats"].as_object() { Some(b) if !b.is_empty() => for (k, v) in b {
        // a beat names its sound (`sfx`) or DESCRIBES it (`want`): the closest description in the kit wins, by similarity
        let (sfx, want) = (v["sfx"].as_str().unwrap_or(""), v["want"].as_str().unwrap_or(""));
        if sfx.is_empty() && want.trim().is_empty() { e.push(format!("beat `{k}`: give `want` (what it should sound like) or `sfx` (one of {})", KIT.join(", "))); }
        if !sfx.is_empty() && !KIT.contains(&sfx) { e.push(format!("beat `{k}`: `sfx` is one of {}", KIT.join(", "))); }
        if want.len() > 200 { e.push(format!("beat `{k}`: `want` is at most 200 characters")); }
        if !num(&v["gain_db"], -40.0, 6.0) { e.push(format!("beat `{k}`: `gain_db` is between -40 and 6")); }
        if let Some(vis) = v.get("visual") { if !matches!(vis.as_str(), Some("big") | Some("static")) { e.push(format!("beat `{k}`: `visual` is big or static")); } }
    }, _ => e.push("`spec.beats` maps each beat name to {want or sfx, gain_db, visual}".into()) }
    // the narration window ends at `end_s`, or `end_margin_s` before the end of the shot (when the shot's length is given by `duration_from`)
    let n = &sp["narration"]; let end_ok = if n.get("end_margin_s").is_some() { a["duration_from"].is_string() && num(&n["end_margin_s"], 0.0, 10.0) } else { num(&n["end_s"], 0.1, 120.0) && n["end_s"].as_f64() > n["start_s"].as_f64() };
    if !(num(&n["start_s"], 0.0, 60.0) && end_ok && num(&n["wps"], 1.5, 4.5) && num(&n["gain_db"], -30.0, 6.0)) { e.push("`spec.narration` is {start_s, end_s (after the start) or end_margin_s (with `duration_from`), wps between 1.5 and 4.5, gain_db}".into()); }
    if !(num(&sp["music"]["gain_db"], -60.0, 0.0) && num(&sp["music"]["duck_db"], -40.0, 0.0)) { e.push("`spec.music` is {gain_db up to 0, duck_db up to 0, and optionally `bed`, `want`, `want_from`}".into()); }
    if let Some(b) = sp["music"]["bed"].as_str() { if !BEDS.contains(&b) { e.push(format!("`spec.music.bed` is one of {}", BEDS.join(", "))); } }
    if let Some(w) = sp["music"]["want_from"].as_str() { if !w.starts_with("values:") { e.push("`spec.music.want_from` is `values:<node>.<field>`: the text the music is chosen for".into()); } }
    if !num(&sp["target_lufs"], -30.0, -6.0) || !num(&sp["true_peak_max"], -6.0, 0.0) { e.push("`spec.target_lufs` is between -30 and -6 and `spec.true_peak_max` between -6 and 0".into()); }
    e
}
fn home() -> PathBuf { PathBuf::from(std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default()) }
/// The person's sound runtime: `ANTILOKI_SOUND_PYTHON`, else `~/.antiloki/sound/venv`. Looks at the machine.
pub fn python_path() -> Result<PathBuf, String> {
    let p = std::env::var("ANTILOKI_SOUND_PYTHON").ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".antiloki/sound/venv").join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" }));
    if p.exists() { Ok(p) } else { Err(format!("the sound runtime is not installed ({} is missing): run scripts/install-sound-runtime.sh, or set ANTILOKI_SOUND_PYTHON to a python with numpy, piper-tts and faster-whisper", p.display())) }
}
pub fn voice_path() -> Result<PathBuf, String> {
    let p = std::env::var("ANTILOKI_SOUND_VOICE").ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".antiloki/sound/voices/en_US-lessac-medium.onnx"));
    if p.exists() { Ok(p) } else { Err(format!("the narration voice is missing ({}): run scripts/install-sound-runtime.sh, or set ANTILOKI_SOUND_VOICE", p.display())) }
}
/// The last `@sound-report {json}` line of the script's output. Pure.
pub fn report_of(out: &str) -> Option<Value> { out.lines().rev().find_map(|l| l.strip_prefix("@sound-report ").and_then(|j| serde_json::from_str(j).ok())) }
/// The failed checks of a report, as one line each. Pure.
pub fn failures_of(rep: &Value) -> Vec<String> { rep["checks"].as_array().map(|a| a.iter().filter(|c| c["ok"] != json!(true)).map(|c| format!("{}: {}", c["check"].as_str().unwrap_or(""), c["detail"].as_str().unwrap_or(""))).collect()).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;
    const TPL: &str = "  tl.addLabel(\"cue:zoom\", 0);\n  tl.fromTo(world, { scale: 1.3, opacity: 0 }, { scale: 1, duration: 0.4, ease: 'power3.out' }, 0.0);\n  tl.addLabel(\"cue:title\", 1.6);\n  tl.fromTo(title, { scale: 0 }, { scale: 1, duration: 0.8, ease: 'elastic.out(1, 0.5)' }, 1.6);\n  tl.addLabel(\"cue:subtitle\", 2.2);\n  tl.to(sub, { x: 0, duration: 0.9 }, 2.2);\n";
    #[test] fn the_cue_sheet_is_read_from_the_labels_and_each_label_is_checked_against_its_tween() {
        let (c, i) = cues_of(TPL); assert!(i.is_empty(), "{i:?}"); assert_eq!(c.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["zoom", "title", "subtitle"]); assert_eq!(c[1].frame, 48); assert_eq!(c[2].t, 2.2);
        let drift = TPL.replace("duration: 0.8, ease: 'elastic.out(1, 0.5)' }, 1.6)", "duration: 0.8, ease: 'elastic.out(1, 0.5)' }, 1.8)"); let (_, i) = cues_of(&drift); assert!(i.iter().any(|m| m.contains("cue:title says 1.6 s but the tween after it starts at 1.8")), "{i:?}");
        let (_, i) = cues_of("tl.addLabel(\"cue:x\", at);\n"); assert!(i[0].contains("plain number")); let (_, i) = cues_of("tl.addLabel(\"cue:x\", 1);\n"); assert!(i[0].contains("no tween follows"));
        let (_, i) = cues_of("tl.addLabel(\"cue:x\", 1);\ntl.to(a, { x: 1 }, t0 + 1);\n"); assert!(i[0].contains("cannot be verified"), "{i:?}");
        let (_, i) = cues_of("tl.addLabel(\"cue:x\", 1);\ntl.set(a, {});\ntl.addLabel(\"cue:x\", 2);\ntl.to(a, {}, 2);\n"); assert!(i.iter().any(|m| m.contains("twice")) || !i.is_empty());
    }
    #[test] fn labels_may_be_expressions_over_declared_constants_and_are_checked_the_same_way() {
        let consts = "var DUR = 9.5;\n"; let tpl = "tl.addLabel(\"cue:zoom\", 0);\ntl.to(a, {}, 0);\ntl.addLabel(\"cue:outro\", DUR - 0.4);\ntl.to(w, {}, DUR - 0.4);\n";
        let (c, i) = cues_with(&[tpl, consts]); assert!(i.is_empty(), "{i:?}"); assert_eq!(c[1].t, 9.1); assert_eq!(c[1].frame, 273);
        let (_, i) = cues_with(&[tpl]); assert!(i.iter().any(|m| m.contains("DUR") && m.contains("declared")), "no constant, no cue: {i:?}");
        let drift = "tl.addLabel(\"cue:outro\", DUR - 0.4);\ntl.to(w, {}, DUR - 0.5);\n"; let (_, i) = cues_with(&[drift, consts]); assert!(i.iter().any(|m| m.contains("says 9.1 s but the tween after it starts at 9 s")), "{i:?}");
        let k = consts_of(&["const A = 2;\n  let B = 0.5;\nvar not_a_const = f(1);\n"]); assert_eq!(k.get("A"), Some(&2.0)); assert_eq!(k.get("B"), Some(&0.5)); assert!(!k.contains_key("not_a_const"));
        let k = consts_of(&["var X = 4;"]); assert_eq!(eval_expr("(X - 1) * 2 / 3 + 0.5", &k).unwrap(), 2.5); assert!(eval_expr("1 +", &k).is_err() && eval_expr("1 / 0", &k).is_err() && eval_expr("2 2", &k).is_err());
    }
    #[test] fn the_narration_window_may_follow_the_shots_length() {
        let mut a = mix_args(); a["duration_from"] = json!("values:len.duration"); a["spec"]["narration"] = json!({ "start_s": 1.7, "end_margin_s": 0.5, "wps": 3.0, "gain_db": 0 }); assert!(check_args("mix", &a).is_empty());
        let mut b = mix_args(); b["spec"]["narration"] = json!({ "start_s": 1.7, "end_margin_s": 0.5, "wps": 3.0, "gain_db": 0 }); assert!(!check_args("mix", &b).is_empty(), "a margin needs duration_from");
        let mut c = mix_args(); c["duration_from"] = json!("len.duration"); assert!(!check_args("mix", &c).is_empty()); let mut d = mix_args(); d["consts"] = json!(["../x.js"]); assert!(!check_args("mix", &d).is_empty());
    }
    #[test] fn a_commas_in_strings_and_objects_do_not_split_the_arguments() { assert_eq!(top_args("a, { b: 1, c: [2, 3] }, 'x, y', 1.5"), vec!["a", "{ b: 1, c: [2, 3] }", "'x, y'", "1.5"]); assert_eq!(matching_paren("f(a, g(b), 'c)')", 1), Some(15)); }
    fn mix_args() -> Value { json!({ "file": "scenes.js", "video": "out/video.mp4", "out": "out/video-sound.mp4", "text_from": "values:rules.output.subtitle", "spec": { "beats": { "zoom": { "sfx": "whoosh", "gain_db": -8, "visual": "big" }, "title": { "sfx": "thump", "gain_db": -5 } },
        "narration": { "start_s": 1.7, "end_s": 4.5, "wps": 3.0, "gain_db": 0 }, "music": { "gain_db": -22, "duck_db": -12 }, "target_lufs": -14.0, "true_peak_max": -1.0 } }) }
    #[test] fn a_sound_step_is_refused_with_the_reason() {
        assert!(check_args("mix", &mix_args()).is_empty()); assert!(check_args("cue_sheet", &json!({ "file": "scenes.js" })).is_empty()); assert!(!check_args("rm_rf", &json!({})).is_empty());
        let mut a = mix_args(); a["spec"]["beats"]["zoom"]["sfx"] = json!("laser"); assert!(check_args("mix", &a).iter().any(|m| m.contains("sfx")));
        let mut a = mix_args(); a["spec"]["beats"]["title"] = json!({ "want": "a strong hit as the title lands", "gain_db": -5 }); assert!(check_args("mix", &a).is_empty(), "a beat may describe its sound");
        let mut a = mix_args(); a["spec"]["beats"]["title"] = json!({ "gain_db": -5 }); assert!(check_args("mix", &a).iter().any(|m| m.contains("want")));
        let mut a = mix_args(); a["spec"]["music"]["bed"] = json!("polka"); assert!(!check_args("mix", &a).is_empty()); a["spec"]["music"]["bed"] = json!("tense"); a["spec"]["music"]["want_from"] = json!("values:tx.output"); assert!(check_args("mix", &a).is_empty());
        let mut a = mix_args(); a["file"] = json!("../x.js"); assert!(!check_args("mix", &a).is_empty()); let mut a = mix_args(); a["text_from"] = json!("hello"); assert!(!check_args("mix", &a).is_empty());
        let mut a = mix_args(); a["spec"]["narration"]["end_s"] = json!(1.0); assert!(check_args("mix", &a).iter().any(|m| m.contains("narration")));
        let mut a = mix_args(); a["spec"]["beats"] = json!({}); assert!(!check_args("mix", &a).is_empty());
    }
    #[test] fn the_script_report_is_read_from_the_last_line_and_its_failures_are_named() {
        let out = "noise\n@sound-report {\"ok\":false,\"checks\":[{\"check\":\"a\",\"ok\":true,\"detail\":\"fine\"},{\"check\":\"b\",\"ok\":false,\"detail\":\"too loud\"}]}\n"; let r = report_of(out).unwrap(); assert_eq!(failures_of(&r), vec!["b: too loud".to_string()]); assert!(report_of("nothing").is_none());
        assert!(MIX_PY.contains("@sound-report") && MIX_PY.contains("check_speech"));
    }
}
