//! antiloki-looks - the bench's door into serve/src/looks.rs (PLAN-THE-DESIGN S2), so the bench driver (bench/design/run.py) and the product share ONE prompt, ONE both-orders rule, ONE ranking.
//!
//!   antiloki-looks prompt --ask TEXT A_DESKTOP A_PHONE B_DESKTOP B_PHONE   the pairwise prompt (design.judge) over four PNG paths
//!   antiloki-looks combine RAW_AB RAW_BA [RAW_THIRD --third-order ab|ba]   two judge answers (files; A=x then A=y) -> the outcome as JSON; exit 3 when either is unread; the optional third settles a split 2 of 3
//!   antiloki-looks version | third-order HASH                              the judge prompt version | which order the third call of a match uses (ab = A is x)
//!   antiloki-looks hash --ask TEXT --judge SEAT PNG x4                     the cache key of a match
//!   antiloki-looks tokens PNG...                                           what one match over these pictures costs a judge (both orders)
//!   antiloki-looks judge-seat --builders e:m,e:m [--available e:m,e:m] [--all]    who may judge; --available stands in for the live "on, signed in, not limited" question
//!   antiloki-looks rank PAIRS.jsonl [--boots N] [--seed N]                 Bradley-Terry / Elo over the rows ({x, y, winner}) -> {method, cells:[{cell, looks, lo, hi, ...}]}
//!   antiloki-looks import PAIRS.jsonl [--runs RUNS.jsonl] [--domain design] [--targets design,frontend]
//!                                                                          store the matches, refit the ranking, and write each seat's looks onto its solo merge rows (look_sum, look_n) - the bench -> router door (S4)
//!   antiloki-looks store PAIRS.jsonl                                       rows -> look_pairs in merges.sqlite (ANTILOKI_HOME says which; a bench points it at its own folder)
use antiloki_serve::cli::{engine_of, Engine};
use antiloki_serve::looks;
use serde_json::{json, Value};
use std::path::PathBuf;

fn arg(a: &[String], k: &str) -> Option<String> { a.iter().position(|x| x == k).and_then(|i| a.get(i + 1).cloned()) }
fn seat(s: &str) -> Option<(Engine, String)> { let (e, m) = s.split_once(':')?; Some((engine_of(&json!(e))?, m.to_string())) }
fn seats(s: &str) -> Vec<(Engine, String)> { s.split(',').filter(|x| !x.trim().is_empty()).filter_map(|x| seat(x.trim())).collect() }
/// the positional arguments: everything that is not a flag or a flag's value
fn positional(a: &[String], with_value: &[&str]) -> Vec<String> {
    let mut out = vec![]; let mut skip = false;
    for x in a { if skip { skip = false; continue } if with_value.contains(&x.as_str()) { skip = true; continue } if x.starts_with("--") { continue } out.push(x.clone()) }
    out
}
fn rows(path: &str) -> Vec<Value> { std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect() }

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let cmd = a.first().map(String::as_str).unwrap_or("help");
    let rest = &a[1.min(a.len())..];
    let r: Result<String, (i32, String)> = match cmd {
        "prompt" => {
            let p = positional(rest, &["--ask"]);
            if p.len() != 4 { Err((2, "prompt needs --ask TEXT and four PNG paths".into())) }
            else { Ok(looks::prompt(&arg(rest, "--ask").unwrap_or_default(), &PathBuf::from(&p[0]), &PathBuf::from(&p[1]), &PathBuf::from(&p[2]), &PathBuf::from(&p[3]))) }
        }
        "combine" => {
            // combine AB BA [THIRD --third-order ab|ba]: THIRD is the v2 third call, used only when AB and BA disagree; "ab" = its A was X
            let p = positional(rest, &["--third-order"]);
            let read = |i: usize| p.get(i).and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| looks::parse_verdict(&t));
            match (read(0), read(1)) {
                (Some(ab), Some(ba)) => {
                    let third = read(2);
                    let o = looks::combine_with_third(&ab, &ba, third.as_ref().map(|v| (v, arg(rest, "--third-order").as_deref() != Some("ba"))));
                    Ok(json!({ "outcome": o, "ab": ab, "ba": ba, "third": third }).to_string())
                }
                (x, y) => Err((3, format!("unread: ab {} / ba {}", if x.is_some() { "ok" } else { "UNREAD" }, if y.is_some() { "ok" } else { "UNREAD" }))),
            }
        }
        "version" => Ok(looks::JUDGE_VERSION.to_string()),
        "third-order" => Ok(if looks::third_a_is_x(rest.first().map(String::as_str).unwrap_or("")) { "ab" } else { "ba" }.to_string()),
        "hash" => {
            let p = positional(rest, &["--ask", "--judge"]);
            let imgs: Vec<Vec<u8>> = p.iter().map(|f| std::fs::read(f).unwrap_or_default()).collect();
            if imgs.len() != 4 || imgs.iter().any(|b| b.is_empty()) { Err((2, "hash needs four readable PNGs".into())) }
            else { Ok(looks::pair_hash(&arg(rest, "--ask").unwrap_or_default(), &imgs, &arg(rest, "--judge").unwrap_or_default())) }
        }
        "tokens" => { let p: Vec<PathBuf> = positional(rest, &[]).into_iter().map(PathBuf::from).collect(); Ok(looks::match_tokens(&p.iter().map(|x| x.as_path()).collect::<Vec<_>>()).to_string()) }
        "judge-seat" => {
            let builders = seats(&arg(rest, "--builders").unwrap_or_default());
            let picks = match arg(rest, "--available") {
                Some(av) => { let av = seats(&av); looks::judge_seats_with(&builders, |e, m| av.iter().any(|(ae, am)| *ae == e && Some(am.as_str()) == m)) }
                None => looks::judge_seats_with(&builders, antiloki_serve::cli::seat_ok),
            };
            // `--all`: every seat that may judge, comma separated, in the gate's order (the bench crosses judges with pairs); else the first
            let all = rest.iter().any(|x| x == "--all");
            let names: Vec<String> = picks.iter().map(|(e, m)| format!("{}:{m}", e.key())).collect();
            if names.is_empty() { Err((4, "no seat can judge this pair today".into())) } else if all { Ok(names.join(",")) } else { Ok(names[0].clone()) }
        }
        "rank" => {
            let p = positional(rest, &["--boots", "--seed"]);
            let ms: Vec<looks::Match> = rows(p.first().map(String::as_str).unwrap_or("")).iter().filter_map(|v| Some(looks::Match::from_winner(v.get("x")?.as_str()?, v.get("y")?.as_str()?, v.get("winner")?.as_str()?))).collect();
            let rk = looks::rank(&ms, arg(rest, "--boots").and_then(|x| x.parse().ok()).unwrap_or(500), arg(rest, "--seed").and_then(|x| x.parse().ok()).unwrap_or(20261001));
            Ok(serde_json::to_string_pretty(&rk).unwrap_or_default())
        }
        "store" => {
            let p = positional(rest, &[]);
            let c = looks::open_db().map_err(|e| (1, e.to_string()));
            c.and_then(|c| {
                let (mut added, mut seen) = (0, 0);
                for v in rows(p.first().map(String::as_str).unwrap_or("")) {
                    let Ok(row) = serde_json::from_value::<looks::PairRow>(v) else { continue };
                    seen += 1; if looks::record_pair(&c, &row).map_err(|e| (1, e.to_string()))? { added += 1 }
                }
                Ok(format!("{added} of {seen} matches stored in {}", antiloki_serve::merges::path().display()))
            })
        }
        "import" => {
            let p = positional(rest, &["--runs", "--domain", "--targets", "--boots", "--seed"]);
            let runs = arg(rest, "--runs").map(|f| rows(&f)).unwrap_or_default();
            let keymap = antiloki_serve::merges::keymap_from_runs(&runs);
            let pair_domain = arg(rest, "--domain").unwrap_or_else(|| "design".into());
            let targets_s = arg(rest, "--targets").unwrap_or_else(|| "design,frontend".into());
            let targets: Vec<&str> = targets_s.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
            let c = antiloki_serve::merges::open_db().map_err(|e| (1, e.to_string()));
            c.and_then(|c| {
                let (mut added, mut seen) = (0, 0);
                for v in rows(p.first().map(String::as_str).unwrap_or("")) {
                    let Ok(row) = serde_json::from_value::<looks::PairRow>(v) else { continue };
                    seen += 1; if looks::record_pair(&c, &row).map_err(|e| (1, e.to_string()))? { added += 1 }
                }
                let imp = antiloki_serve::merges::import_looks(&c, &pair_domain, &keymap, &targets, arg(rest, "--boots").and_then(|x| x.parse().ok()).unwrap_or(500), arg(rest, "--seed").and_then(|x| x.parse().ok()).unwrap_or(20261001), looks::calibrated_judge().as_deref()).map_err(|e| (1, e.to_string()))?;
                Ok(json!({ "stored": added, "of": seen, "file": antiloki_serve::merges::path().display().to_string(), "import": imp }).to_string())
            })
        }
        _ => Err((2, "usage: antiloki-looks prompt|combine|hash|tokens|judge-seat|rank|store|import (see the header of serve/src/bin/antiloki-looks.rs)".into())),
    };
    match r { Ok(s) => println!("{s}"), Err((code, e)) => { eprintln!("antiloki-looks: {e}"); std::process::exit(code) } }
}
