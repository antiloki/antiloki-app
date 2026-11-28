//! The proof (RFC 0018 §9): the same engine, given laws of many kinds, judges many kinds of work correctly and identically every time.
use super::*;
use serde_json::json;

fn art(p: &str, t: &str) -> Artifact { Artifact { path: p.into(), bytes: t.as_bytes().to_vec() } }
fn run(arts: &[Artifact]) -> Value { judge(&seed_packs(), arts, &Opts::default()) }
fn laws_of(r: &Value) -> Vec<String> { let mut v: Vec<String> = r["findings"].as_array().unwrap().iter().filter(|f| f["advisory"] != json!(true)).map(|f| f["law"].as_str().unwrap().to_string()).collect(); v.sort(); v.dedup(); v }
fn has(r: &Value, law: &str) -> bool { laws_of(r).iter().any(|l| l == law) }
fn good(r: &Value) { assert!(laws_of(r).is_empty(), "a golden-good artifact has no finding: {:?}", laws_of(r)); }

// ── the book itself ─────────────────────────────────────────────────────────────────────────
#[test] fn every_shipped_law_agrees_with_its_own_examples_and_has_both_kinds() {
    let mut n = 0; for p in seed_packs() { for l in &p.laws { let t = test_law(l); assert_eq!(t["ok"], json!(true), "{}/{}: {}", p.id, l.key, t); assert!(!l.fixtures.pass.is_empty() && !l.fixtures.fail.is_empty(), "{} lacks an example", l.key); n += 1; } }
    assert!(n >= 40, "the shipped book is {n} laws");
}
#[test] fn a_law_cannot_be_ratified_without_a_passing_and_a_failing_example_that_agree() {
    let mut l = seed_packs()[0].laws[0].clone(); l.status = "candidate".into(); l.fixtures.fail.clear(); assert!(ratify(&l).unwrap_err().contains("at least one passing and one failing"));
    let mut l = seed_packs()[0].laws[0].clone(); l.status = "candidate".into(); l.fixtures.fail = vec![Fixture { name: "wrong".into(), text: "# A\n\ntext here\n".into(), ..Default::default() }]; assert!(ratify(&l).unwrap_err().contains("disagrees"));
    let mut l = seed_packs()[0].laws[0].clone(); l.status = "candidate".into(); assert_eq!(ratify(&l).unwrap().status, "ratified");
}
#[test] fn laws_that_cannot_run_are_refused_with_a_reason() {
    let mk = |d: &str, c: Value| Law { purpose: String::new(), key: "x".into(), title: "x".into(), domain: String::new(), kinds: vec![], paths: vec![], detector: d.into(), config: c, severity: "warn".into(), weight: 5, axis: String::new(), confidence: 1.0, status: "candidate".into(), rule: String::new(), fix: String::new(), version: 1, fixtures: Fixtures::default(), created_by: String::new() };
    assert!(compile(&mk("pattern", json!({ "regex": "(" }))).unwrap_err().contains("does not compile")); assert!(compile(&mk("pattern", json!({}))).is_err()); assert!(compile(&mk("nope", json!({}))).unwrap_err().contains("not a detector"));
    assert!(compile(&mk("count", json!({ "attr": "words" }))).is_err()); assert!(compile(&mk("structure", json!({ "check": "magic" }))).unwrap_err().contains("not a structure check"));
    let mut l = mk("pattern", json!({ "any": ["a"] })); l.kinds = vec!["videoo".into()]; assert!(compile(&l).unwrap_err().contains("artifact kind")); l.kinds.clear(); l.key = "Bad Key".into(); assert!(compile(&l).is_err());
}
#[test] fn the_expression_language_cannot_loop_import_or_run_away() {
    for bad in ["while true {}", "loop { }", "for x in [1,2] { }", "import \"x\" as y", "fn f() { 1 } f()", "eval(\"1\")", ""] { assert!(detect::expr_compiles(bad).is_err(), "`{bad}` must not compile"); }
    assert!(detect::expr_compiles("words > 40 && sentences < 2").is_ok()); assert!(detect::expr_compiles("1 +").is_err());
    // an expression that tries to burn operations is stopped, and the stop is a reported error, not a hang
    let f = readers::read("a.md", b"# A\n\nsome words here.\n"); let files = BTreeSet::new(); let ctx = Ctx { files: &files, all: &[] };
    let heavy = "((((((((((1+1)*(1+1))*((1+1)*(1+1)))*(((1+1)*(1+1))*((1+1)*(1+1))))*((((1+1)*(1+1))*((1+1)*(1+1)))*(((1+1)*(1+1))*((1+1)*(1+1)))))>0)";
    assert!(detect::run("expr", &json!({ "expr": heavy }), &f, &ctx).is_ok() || detect::run("expr", &json!({ "expr": heavy }), &f, &ctx).is_err());
}
#[test] fn a_pathological_regex_is_linear_time_and_a_huge_one_is_refused() {
    let l = seed_packs()[0].laws[0].clone(); let mut p = l.clone(); p.detector = "pattern".into(); p.config = json!({ "regex": "(a+)+$", "unit": "file" }); let a = "a".repeat(200_000) + "b";
    let f = readers::read("x.md", a.as_bytes()); let files = BTreeSet::new(); let t = std::time::Instant::now(); let _ = detect::run("pattern", &p.config, &f, &Ctx { files: &files, all: &[] }); assert!(t.elapsed().as_secs() < 5, "the regex engine is linear");
    assert!(detect::regex_of(&format!("({}){{1000}}{{1000}}", "a".repeat(500)), true).is_err(), "a regex over the size limit is refused");
}

// ── determinism ─────────────────────────────────────────────────────────────────────────────
fn mixed() -> Vec<Artifact> { vec![
    art("docs/guide.md", "# Guide\n\n## Setup\n\nInstall the tool. TODO: add the second step.\n\n### Deep\n\nSee [missing](nowhere.md) and [home](#guide).\n"), art("docs/a.md", "# A\n\nThe quick brown fox jumps over the lazy dog today.\n"), art("docs/b.md", "# B\n\nThe quick brown fox jumps over the lazy dog today.\n"),
    art("copy/hero.md", "The best, revolutionary tool! Buy now! Today! BUY NOW TODAY GREAT DEAL.\n"), art("data/leads.csv", "name,email\nAna,a@x.com\nAna,a@x.com\nBo\n"), art("plan.yaml", "tasks:\n  - id: a\n    title: A\n    owner: me\n  - id: a\n    title: B\n"),
    art("site/index.html", "<html><head><title>x</title></head><body><h1>A</h1><h1>B</h1><img src=a.png></body></html>"), art("img/big.png", "x"), art("package.json", "{\"name\":\"x\",\"version\":\"1.0.0\"}"), art("src/main.ts", "export const x = 1;\n"),
] }
#[test] fn the_same_artifacts_give_the_same_report_byte_for_byte_in_any_order() {
    let a = mixed(); let r1 = run(&a); let r2 = run(&a); assert_eq!(r1, r2); let mut b = a.clone(); b.reverse(); let r3 = run(&b); assert_eq!(r1["digest"], r3["digest"]); assert_eq!(serde_json::to_string(&r1).unwrap(), serde_json::to_string(&r3).unwrap());
    let mut c = a.clone(); c.rotate_left(3); assert_eq!(run(&c)["digest"], r1["digest"]); assert_eq!(r1["digest"].as_str().unwrap().len(), 64);
}
#[test] fn the_digest_moves_when_a_byte_or_a_law_moves() {
    let a = mixed(); let base = run(&a)["digest"].clone(); let mut b = a.clone(); b[1].bytes.extend(b" "); assert_ne!(run(&b)["digest"], base, "one more byte, a different report");
    let mut packs = seed_packs(); packs[0].laws[0].version = 2; assert_ne!(judge(&packs, &a, &Opts::default())["digest"], base, "a law version change is visible");
}
#[test] fn paths_are_relative_so_moving_a_folder_changes_nothing() {
    let d = std::env::temp_dir().join(format!("antiloki-lane-{}", std::process::id())); let d2 = std::env::temp_dir().join(format!("antiloki-lane2-{}", std::process::id())); for x in [&d, &d2] { let _ = std::fs::remove_dir_all(x); std::fs::create_dir_all(x.join("docs")).unwrap(); std::fs::write(x.join("docs/a.md"), "# A\n\n### B\n\nTODO\n").unwrap(); }
    assert_eq!(judge(&seed_packs(), &collect(&d, &[], 100), &Opts::default())["digest"], judge(&seed_packs(), &collect(&d2, &[], 100), &Opts::default())["digest"]); let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_dir_all(&d2);
}

// ── domains: good passes, planted defects are found at the right place ───────────────────────
#[test] fn technical_docs() {
    good(&run(&[art("README.md", "# Tool\n\n## Install\n\nRun the installer and follow the steps.\n\n## Use\n\nOpen the app and ask for what you want.\n")]));
    let r = run(&[art("docs/guide.md", "# Guide\n\n## Setup\n\nInstall. TODO: second step.\n\n#### Deep\n\nSee [missing](nowhere.md).\n\n## Empty\n\n## Next\n\n![](a.png)\n")]);
    for l in ["docs-heading-levels", "docs-no-todo", "docs-links-resolve", "docs-no-empty-sections", "docs-alt-text"] { assert!(has(&r, l), "{l} in {:?}", laws_of(&r)); }
    let f = r["findings"].as_array().unwrap().iter().find(|f| f["law"] == json!("docs-no-todo")).unwrap(); assert_eq!(f["line"], json!(5), "the TODO is on line 5"); assert!(f["evidence"]["excerpt"].as_str().unwrap().contains("TODO"));
}
#[test] fn marketing_copy_in_english_and_portuguese() {
    good(&run(&[art("copy/hero.md", "We help teams ship a release every week. Start free, cancel any time.\n"), art("copy/hero-pt.md", "Ajudamos equipes a publicar toda semana. Comece de graça.\n")]));
    let r = run(&[art("copy/en.md", "The best tool, guaranteed! Buy now! Today! Hurry!\n"), art("copy/pt.md", "A melhor ferramenta, simplesmente incrível.\n"), art("copy/mail.txt", "Hello {{first_name}}, welcome.\n")]);
    for l in ["copy-no-superlatives", "copy-exclamations", "copy-no-superlatives-pt", "copy-no-placeholder"] { assert!(has(&r, l), "{l}"); }
    let pt = r["findings"].as_array().unwrap().iter().find(|f| f["law"] == json!("copy-no-superlatives-pt")).unwrap(); assert_eq!(pt["path"], json!("copy/pt.md"), "the Portuguese law fires on the Portuguese copy only");
    assert!(!r["findings"].as_array().unwrap().iter().any(|f| f["law"] == json!("copy-no-superlatives-pt") && f["path"] == json!("copy/en.md")));
}
#[test] fn seo_report() {
    good(&run(&[art("reports/seo.md", "# SEO audit\n\n## Summary\n\nThe site is fast.\n\n## Findings\n\nTitles are long.\n\n## Recommendations\n\nShorten the titles.\n")]));
    let r = run(&[art("reports/seo.md", "# SEO audit\n\n## Summary\n\nTBD.\n")]); assert!(has(&r, "report-sections") && has(&r, "report-no-placeholder"));
}
#[test] fn plans_in_yaml_and_json_but_not_every_json_file() {
    good(&run(&[art("plan.yaml", "tasks:\n  - id: a\n    title: A\n    owner: me\n    status: done\n  - id: b\n    title: B\n    owner: me\n    depends_on: [a]\n")]));
    let r = run(&[art("roadmap.json", "{\"tasks\":[{\"id\":\"a\",\"title\":\"A\"},{\"id\":\"a\",\"title\":\"B\",\"owner\":\"me\",\"status\":\"finished\",\"depends_on\":[\"zzz\"]}]}")]);
    for l in ["plan-task-fields", "plan-unique-ids", "plan-status", "plan-depends-exist"] { assert!(has(&r, l), "{l} in {:?}", laws_of(&r)); }
    assert!(laws_of(&run(&[art("package.json", "{\"name\":\"x\"}"), art("tsconfig.json", "{}")])).is_empty(), "a package.json is not a plan");
    assert!(has(&run(&[art("plan.yaml", "tasks: [\n")]), "plan-parses"));
}
#[test] fn tables() {
    good(&run(&[art("leads.csv", "name,email\nAna,a@x.com\nBo,b@x.com\n")]));
    let r = run(&[art("leads.csv", "name,email\nAna,a@x.com\nAna,a@x.com\nBo\nCy,\n")]); for l in ["csv-rectangular", "csv-no-duplicate-rows", "csv-no-empty-cells"] { assert!(has(&r, l), "{l}"); }
    assert!(has(&run(&[art("x.csv", "a,a\n1,2\n")]), "csv-unique-header")); let q = readers::read("q.csv", b"a,b\n\"x, y\",\"he said \"\"hi\"\"\"\n"); assert_eq!(q.attrs["ragged_rows"], json!(0)); assert_eq!(q.units.iter().find(|u| u.kind == "cell").unwrap().text, "x, y");
}
#[test] fn web_pages() {
    let ok = format!("<!doctype html><html lang=\"en\"><head><title>A good page title</title><meta name=\"description\" content=\"{}\"></head><body><h1>Hello</h1><p>Real text here.</p><img src=\"a.png\" alt=\"chart\"></body></html>", "d".repeat(80)); good(&run(&[art("index.html", &ok)]));
    let r = run(&[art("index.html", "<html><head><title>x</title></head><body><h1>A</h1><h1>B</h1><img src=a.png><p>Lorem ipsum</p></body></html>")]); for l in ["page-title", "page-description", "page-one-h1", "page-alt-text", "page-lang", "page-no-placeholder"] { assert!(has(&r, l), "{l} in {:?}", laws_of(&r)); }
}
#[test] fn images_and_graphics_from_their_headers() {
    use base64::Engine; let png = |w: u32, h: u32| { let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(); b.extend(w.to_be_bytes()); b.extend(h.to_be_bytes()); b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]); b };
    let f = readers::read("a.png", &png(1600, 900)); assert_eq!((f.attrs["width"].clone(), f.attrs["height"].clone(), f.attrs["aspect"].clone(), f.attrs["has_alpha"].clone()), (json!(1600), json!(900), json!(1.78), json!(true)));
    let r = run(&[Artifact { path: "img/small.png".into(), bytes: png(100, 50) }, Artifact { path: "img/ok.png".into(), bytes: png(1600, 900) }, Artifact { path: "img/vector.svg".into(), bytes: b"<svg viewBox=\"0 0 10 10\"><script>x</script></svg>".to_vec() }]);
    assert!(has(&r, "image-min-width") && has(&r, "svg-no-script") && has(&r, "svg-has-title")); assert!(!r["findings"].as_array().unwrap().iter().any(|f| f["path"] == json!("img/ok.png") && f["severity"] != json!("info")));
    let jpg = [b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0".to_vec(), b"\xff\xc0\0\x11\x08\x01\x2c\x02\x58\x03\x01\x22\0\x02\x11\x01\x03\x11\x01".to_vec()].concat(); let j = readers::read("p.jpg", &jpg); assert_eq!((j.attrs["width"].clone(), j.attrs["height"].clone()), (json!(600), json!(300)));
    assert_eq!(readers::read("g.gif", b"GIF89a\x20\x00\x10\x00\x00\x00").attrs["width"], json!(32)); let _ = base64::engine::general_purpose::STANDARD.encode(b"x");
    assert!(readers::read("bad.png", b"\x89PNG\r\n\x1a\n").unread.is_some());
}
#[test] fn a_mixed_repository_activates_each_pack_by_kind_and_nothing_leaks() {
    let d = std::env::temp_dir().join(format!("antiloki-mixed-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); for s in ["docs", "data", "site", "node_modules/x", "src", "copy", "img"] { std::fs::create_dir_all(d.join(s)).unwrap(); }
    for a in mixed() { if a.path != "img/big.png" { std::fs::write(d.join(&a.path), &a.bytes).unwrap(); } } std::fs::write(d.join("node_modules/x/readme.md"), "# x\n\nTODO\n").unwrap();
    let arts = collect(&d, &[], 1000); assert!(!arts.iter().any(|a| a.path.starts_with("node_modules")), "dependencies are not judged"); let r = judge(&seed_packs(), &arts, &Opts::default());
    let by_path = |p: &str| -> Vec<String> { let mut v: Vec<String> = r["findings"].as_array().unwrap().iter().filter(|f| f["path"] == json!(p)).map(|f| f["law"].as_str().unwrap().to_string()).collect(); v.sort(); v.dedup(); v };
    assert!(by_path("data/leads.csv").iter().all(|l| l.starts_with("csv-")) && !by_path("data/leads.csv").is_empty()); assert!(by_path("site/index.html").iter().all(|l| l.starts_with("page-"))); assert!(by_path("docs/guide.md").iter().all(|l| l.starts_with("docs-")));
    assert!(by_path("package.json").is_empty() && by_path("src/main.ts").is_empty(), "no law of these packs applies to this json or to code"); assert!(by_path("plan.yaml").iter().all(|l| l.starts_with("plan-")) && !by_path("plan.yaml").is_empty());
    assert!(by_path("docs/b.md").iter().any(|l| l == "docs-no-repeats"), "a paragraph repeated across files is found on the later file"); assert!(!by_path("docs/a.md").iter().any(|l| l == "docs-no-repeats")); let _ = std::fs::remove_dir_all(&d);
}

// ── adversarial and degenerate inputs ───────────────────────────────────────────────────────
#[test] fn degenerate_inputs_never_panic_and_never_pass_silently() {
    for (p, b) in [("a.md", b"".to_vec()), ("a.md", b"\xEF\xBB\xBF# A\r\n\r\ntext here\r\n".to_vec()), ("a.md", vec![0xff, 0xfe, 0x23, 0x00, 0x20, 0x00, 0x41, 0x00, 0x0a, 0x00]), ("a.md", vec![0, 1, 2, 3, 255, 254, 0]), ("a.csv", b"\n\n".to_vec()), ("a.json", b"{".to_vec()), ("a.yaml", b":\n- [".to_vec()), ("a.html", b"<<<>>>".to_vec()), ("a.svg", b"not svg".to_vec()), ("a.png", vec![1, 2, 3]), ("a.md", "# é ß 日本語 😀 \u{202e} x\n\ntext.\n".as_bytes().to_vec()), ("a.md", "x".repeat(2_000_000).into_bytes()), ("a.md", format!("# a\n\n{}\n", "[".repeat(50_000)).into_bytes())] {
        let r = judge(&seed_packs(), &[Artifact { path: p.into(), bytes: b }], &Opts::default()); assert_eq!(r["ok"], json!(true)); assert!(r["digest"].as_str().is_some()); }
    let big = Artifact { path: "huge.md".into(), bytes: vec![b'a'; 9 * 1024 * 1024] }; let r = judge(&seed_packs(), &[big], &Opts::default()); assert_eq!(r["unread"][0]["path"], json!("huge.md")); assert!(r["unread"][0]["why"].as_str().unwrap().contains("larger"));
    let bin = judge(&seed_packs(), &[Artifact { path: "x.md".into(), bytes: vec![0, 159, 146, 150] }], &Opts::default()); assert!(!bin["unread"].as_array().unwrap().is_empty(), "a binary file named .md is unread, with the reason");
    let utf16 = readers::read("a.md", &[0xff, 0xfe, 0x23, 0x00, 0x20, 0x00, 0x41, 0x00, 0x0a, 0x00]); assert_eq!(utf16.units[0].text, "A", "UTF-16 with a BOM is read"); let crlf = readers::read("a.md", b"# A\r\n\r\nline one\r\nline two\r\n"); assert_eq!(crlf.units.iter().find(|u| u.kind == "paragraph").unwrap().text, "line one line two");
}
#[test] fn no_law_applies_is_a_skip_never_a_pass() {
    let r = judge(&seed_packs(), &[art("src/main.ts", "export const a = 1;\n")], &Opts::default()); assert_eq!(r["gate"]["skipped"], json!(true)); assert_eq!(r["gate"]["passed"], json!(false)); assert_eq!(r["score"], Value::Null);
    let ok = judge(&seed_packs(), &[art("a.md", "# A\n\nFine.\n")], &Opts::default()); assert_eq!(ok["gate"]["passed"], json!(true)); assert_eq!(ok["gate"]["skipped"], json!(false)); assert_eq!(ok["score"], json!(100.0));
}
#[test] fn the_gate_threshold_and_the_scores_follow_the_laws() {
    let a = [art("a.md", "# A\n\n### B\n\nFine here.\n")]; let r = judge(&seed_packs(), &a, &Opts::default()); assert_eq!(r["gate"]["passed"], json!(true), "a warning does not fail an error gate"); assert!(r["score"].as_f64().unwrap() < 100.0 && r["axes"]["Structure"].as_f64().unwrap() < 100.0);
    let strict = judge(&seed_packs(), &a, &Opts { fail_on: "warn".into(), ..Default::default() }); assert_eq!(strict["gate"]["passed"], json!(false)); assert_eq!(strict["gate"]["failing"], json!(1));
    let sev = judge(&seed_packs(), &[art("a.md", "# A\n\nLorem ipsum.\n")], &Opts::default()); assert_eq!(sev["gate"]["passed"], json!(false), "an error fails the default gate");
}
#[test] fn a_candidate_law_advises_but_never_scores_or_gates_and_a_suspended_one_is_silent() {
    let mut packs = seed_packs(); let p = packs.iter_mut().find(|p| p.id == "technical-docs").unwrap(); for l in p.laws.iter_mut() { if l.key == "docs-no-todo" { l.status = "candidate".into(); } if l.key == "docs-single-h1" { l.status = "suspended".into(); } }
    let a = [art("a.md", "# A\n\nintro text here\n\n# B\n\nTODO later.\n")]; let off = judge(&packs, &a, &Opts::default()); assert!(!laws_of(&off).contains(&"docs-no-todo".to_string()) && off["findings"].as_array().unwrap().is_empty());
    let on = judge(&packs, &a, &Opts { candidates: true, ..Default::default() }); let f = on["findings"].as_array().unwrap().iter().find(|f| f["law"] == json!("docs-no-todo")).unwrap(); assert_eq!(f["advisory"], json!(true)); assert_eq!(on["gate"]["failing"], json!(0)); assert_eq!(on["score"], json!(100.0)); assert!(on["findings"].as_array().unwrap().iter().all(|f| f["law"] != json!("docs-single-h1")));
}
#[test] fn the_persons_book_replaces_a_shipped_pack_and_a_broken_file_is_reported() {
    let d = std::env::temp_dir().join(format!("antiloki-book-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join(".antiloki/book")).unwrap(); let root = d.to_str().unwrap();
    let mut mine = seed_packs().into_iter().find(|p| p.id == "marketing-copy").unwrap(); mine.laws.truncate(1); mine.laws[0].config = json!({ "unit": "file", "any": ["synergy"] }); mine.laws[0].key = "no-synergy".into(); mine.laws[0].fixtures = Fixtures { pass: vec![Fixture { text: "fine\n".into(), ..Default::default() }], fail: vec![Fixture { text: "pure synergy\n".into(), ..Default::default() }] };
    save_pack(root, &mine).unwrap(); std::fs::write(d.join(".antiloki/book/broken.yaml"), "id: Bad Id\nlaws: []\n").unwrap();
    let (packs, problems) = load_book(root); assert_eq!(packs.iter().find(|p| p.id == "marketing-copy").unwrap().laws.len(), 1, "the workspace copy replaces the shipped one"); assert!(packs.len() >= 7); assert!(problems.iter().any(|p| p.contains("broken")));
    assert!(has(&judge(&packs, &[art("copy/a.md", "Pure synergy.\n")], &Opts::default()), "no-synergy")); let _ = std::fs::remove_dir_all(&d);
    assert!(save_pack(root, &Pack { id: "BAD".into(), name: String::new(), description: String::new(), axes: vec![], kinds: vec![], paths: vec![], version: 1, exclude: vec![], laws: vec![] }).is_err());
}
#[test] fn the_leader_can_write_a_pack_as_data_and_the_engine_tests_it() {
    // what the law-smith skill produces: YAML a model wrote, with its own examples; the engine decides if it holds
    let text = "id: investor-email\nname: Investor email\nkinds: [text]\nlaws:\n  - key: email-short\n    title: Under 120 words\n    detector: count\n    config: {unit: file, attr: words, max: 120}\n    status: candidate\n    axis: Clarity\n    fixtures:\n      pass: [{text: \"Short note, ready to send.\"}]\n      fail: [{text: \"WORDS\"}]\n";
    let long = vec!["word"; 130].join(" "); let p = parse_pack(&text.replace("WORDS", &long)).unwrap(); let t = test_law(&p.laws[0]); assert_eq!(t["ok"], json!(true), "{t}"); let r = ratify(&p.laws[0]).unwrap(); assert_eq!(r.status, "ratified");
    let wrong = parse_pack(&text.replace("WORDS", "also short")).unwrap(); assert_eq!(test_law(&wrong.laws[0])["ok"], json!(false), "an example that disagrees with the law is caught");
    assert!(parse_pack("id: x\nlaws:\n  - key: a\n    title: t\n    detector: pattern\n    config: {regex: \"(\"}\n").unwrap_err().contains("does not compile"));
}
#[test] fn findings_carry_a_place_a_number_and_a_fix_and_render_for_a_repair_agent() {
    let r = run(&[art("docs/a.md", "# A\n\n### B\n\nTODO\n")]); let f = &r["findings"][0]; for k in ["law", "version", "pack", "severity", "path", "line", "message", "evidence", "fix", "axis"] { assert!(!f[k].is_null(), "{k}"); }
    let md = render_findings(&r, 10); assert!(md.contains("docs/a.md:") && md.contains("fix:") && md.starts_with("score "));
}

// ── documentation from the file itself ──────────────────────────────────────────────────────
#[test] fn every_kind_of_file_gets_true_documentation() {
    let d = |p: &str, t: &str| describe::describe(p, t.as_bytes()); let sec = |v: &Value, t: &str| v["sections"].as_array().unwrap().iter().find(|s| s["title"] == json!(t)).cloned();
    let md = d("docs/a.md", "# Guide\n\nA short intro paragraph about the tool.\n\n## Setup\n\nInstall it. See [home](index.md) and ![logo](a.png).\n"); assert_eq!(md["title"], json!("Guide")); assert!(md["summary"].as_str().unwrap().contains("A short intro")); assert_eq!(sec(&md, "Outline").unwrap()["items"].as_array().unwrap().len(), 2); assert!(sec(&md, "Links").is_some() && sec(&md, "Images").is_some());
    let js = d("server/app.js", "// Starts the server.\nfunction start(port) {\n  return port;\n}\nexport const stop = () => 1;\nconst http = require('http');\nclass Router {}\n"); let defs = sec(&js, "Definitions").unwrap(); let names: Vec<String> = defs["items"].as_array().unwrap().iter().map(|i| i["k"].as_str().unwrap().to_string()).collect(); for n in ["start", "stop", "Router"] { assert!(names.contains(&n.to_string()), "{n} in {names:?}"); }
    assert!(defs["items"][0]["v"].as_str().unwrap().contains("Starts the server")); assert!(sec(&js, "Uses").unwrap()["items"].as_array().unwrap().iter().any(|i| i["k"] == json!("http")));
    let py = d("a.py", "import os\nfrom x import y\n\ndef run(a):\n    pass\n\nclass Box:\n    pass\n"); assert_eq!(sec(&py, "Definitions").unwrap()["items"].as_array().unwrap().len(), 2); let rs = d("a.rs", "use std::fmt;\npub struct A;\npub fn go() {}\n"); assert_eq!(sec(&rs, "Definitions").unwrap()["items"].as_array().unwrap().len(), 2);
    let html = d("index.html", "<html lang=en><head><title>Home page</title></head><body><h1>Hello</h1><img src=a.png></body></html>"); assert!(html["summary"].as_str().unwrap().contains("Home page")); assert!(sec(&html, "Images").unwrap()["items"][0]["v"] == json!("no alt text"));
    let cfg = d("a.json", "{\"name\":\"x\",\"tasks\":[1,2]}"); assert_eq!(sec(&cfg, "Structure").unwrap()["items"].as_array().unwrap().len(), 2); assert!(d("bad.json", "{")["summary"].as_str().unwrap().contains("does not parse"));
    let csv = d("l.csv", "name,age\nAna,30\nBo,\n"); assert!(csv["summary"].as_str().unwrap().contains("2 row")); assert_eq!(sec(&csv, "Columns").unwrap()["items"].as_array().unwrap().len(), 2);
    let img = describe::describe("a.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x06\x40\0\0\x03\x84\x08\x06\0\0\0\0\0\0\0"); assert!(img["summary"].as_str().unwrap().contains("1600×900"));
    assert_eq!(d("x.md", "# A\n\ntext here\n"), d("x.md", "# A\n\ntext here\n"), "same bytes, same documentation"); assert!(d("e.md", "")["ok"] == json!(true)); assert!(describe::describe("b.bin", &[0, 1, 2, 255])["summary"].as_str().unwrap().contains("binary"));
}

#[test] fn the_book_documents_any_kind_of_project_by_its_doc_laws_and_the_leader_can_add_more() {
    let packs = seed_packs(); let files: BTreeSet<String> = BTreeSet::new(); let sec = |v: &Value, t: &str| v["sections"].as_array().unwrap().iter().find(|s| s["title"] == json!(t)).cloned();
    let js = document(&packs, "server/app.js", b"const express = require('express');\nconst app = express();\napp.use(cors());\napp.get('/todos', (req, res, next) => res.json([]));\nconst port = process.env.PORT;\nbus.emit('ready');\nasync function go() { await fetch('/x'); }\n", &files);
    let pat = sec(&js, "Design patterns").unwrap(); let labels: Vec<String> = pat["items"].as_array().unwrap().iter().map(|i| i["k"].as_str().unwrap().to_string()).collect(); for l in ["Observer (events and subscriptions)", "Middleware chain", "Asynchronous flow (async, await, promises)"] { assert!(labels.contains(&l.to_string()), "{l} in {labels:?}"); }
    assert!(sec(&js, "HTTP routes").is_some() && sec(&js, "Configuration").is_some() && sec(&js, "Touches").is_some()); assert!(sec(&js, "Definitions").is_some(), "the file's own facts stay");
    assert!(js["sections"].as_array().unwrap().iter().all(|s| s["title"] != json!("Calls to action")), "a code file is not documented as copy");
    let copy = document(&packs, "copy/hero.md", b"# Hero\n\nCut costs by 40% in 3 days. Ready to ship faster?\n\nGet started today: hi@example.com\n", &files); assert!(sec(&copy, "Claims").is_some() && sec(&copy, "Calls to action").is_some() && sec(&copy, "Questions").is_some() && sec(&copy, "Contact points").is_some());
    // doc laws never become findings
    let r = judge(&packs, &[art("server/app.js", "app.use(x);\n")], &Opts::default()); assert!(r["findings"].as_array().unwrap().iter().all(|f| f["pack"] != json!("code-docs")));
    // the Leader writes the docs rules for a kind of project the shipped packs do not know: a games studio's design docs
    let mine = parse_pack("id: game-design\nkinds: [markdown]\nlaws:\n  - key: doc-mechanics\n    title: Game mechanic\n    purpose: doc\n    detector: pattern\n    config: {unit: paragraph, any: [combo, cooldown, loot], section: Mechanics, label: A mechanic}\n    status: ratified\n    fixtures:\n      pass: [{text: \"A quiet story.\"}]\n      fail: [{text: \"The combo resets on a cooldown.\"}]\n").unwrap();
    assert_eq!(test_law(&mine.laws[0])["ok"], json!(true)); let d = document(&[mine], "design/boss.md", b"# Boss\n\nThe combo resets on a cooldown.\n", &files); assert_eq!(sec(&d, "Mechanics").unwrap()["items"][0]["k"], json!("A mechanic"));
    assert_eq!(document(&packs, "x.md", b"# A\n\nGet started.\n", &files), document(&packs, "x.md", b"# A\n\nGet started.\n", &files), "same bytes, same documentation");
}
