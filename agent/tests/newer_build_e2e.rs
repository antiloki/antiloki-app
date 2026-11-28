//! An install while an agent is open: its tool helper becomes the new build, keeps its process and pipes, and answers the request that found the change.
#![cfg(unix)]
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn a_running_helper_becomes_the_newly_installed_build() {
    let dir = std::env::temp_dir().join(format!("antiloki-newer-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
    for build in ["one", "two", "broken"] { std::fs::create_dir_all(dir.join(build)).unwrap(); }
    for build in ["one", "two"] { std::fs::copy(env!("CARGO_BIN_EXE_antiloki-agent"), dir.join(build).join("antiloki-agent")).unwrap(); }
    std::fs::write(dir.join("broken/antiloki-agent"), "half a file").unwrap();
    let install = |build: &str| { let link = dir.join("main"); let _ = std::fs::remove_file(&link); std::os::unix::fs::symlink(dir.join(build), &link).unwrap(); };
    install("one");
    let mut child = Command::new(dir.join("main/antiloki-agent")).arg("mcp").env("ANTILOKI_BE_URL", "http://127.0.0.1:9").env("ANTILOKI_PANE_ID", "7").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let pid = child.id(); let mut to = child.stdin.take().unwrap(); let mut from = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |id: u32, method: &str| writeln!(to, "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"}}").unwrap();
    let mut next = || { let mut l = String::new(); from.read_line(&mut l).unwrap(); serde_json::from_str::<serde_json::Value>(&l).unwrap() };
    let running = || std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    ask(1, "initialize"); assert_eq!(next()["result"]["capabilities"]["tools"]["listChanged"], serde_json::json!(true));
    // a file that cannot run is not taken
    install("broken"); std::thread::sleep(std::time::Duration::from_millis(100)); ask(2, "tools/list"); assert_eq!(next()["id"], serde_json::json!(2));
    install("two"); std::thread::sleep(std::time::Duration::from_millis(100)); ask(3, "tools/list");
    assert_eq!(next()["method"], serde_json::json!("notifications/tools/list_changed"));
    let answer = next(); assert_eq!(answer["id"], serde_json::json!(3), "the request that found the new build is answered by it"); assert!(!answer["result"]["tools"].as_array().unwrap().is_empty());
    if cfg!(target_os = "linux") { assert!(running().is_some_and(|p| p.ends_with("two/antiloki-agent")), "same process, new program: {:?}", running()); }
    ask(4, "tools/list"); assert_eq!(next()["id"], serde_json::json!(4), "and it goes on from there, without switching again");
    drop(ask); drop(to); assert!(child.wait().unwrap().success()); let _ = std::fs::remove_dir_all(&dir);
}
