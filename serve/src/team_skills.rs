//! A TEAM'S OWN SKILLS (RFC 0008 §4.16). The Leader writes the skills a custom agent needs and that do not exist; each is a markdown document in the team's folder,
//! `<project>/.antiloki/teams/<team>/skills/<name>.md`, with a small header (purpose, the effects it declares, its review status) and the playbook an agent follows.
//! A skill is a document an agent READS: the serve never runs it, and it cannot widen what the agent may do (a skill that declares an effect its agent lacks is an error). A skill the Leader writes is APPROVED BY THE LEADER BY DEFAULT
//! (the operator 2026-11-18: "skills suggested should be approved by the leader by default"); the person can still remove it, and a file marked `draft` (by hand, or by an older run) still has to be approved by the person before a run uses it.

use crate::team_graph::{validate, Graph, Kind, Report, RoleKind, AGENT_EFFECTS};
use serde::Serialize;
use std::path::{Path, PathBuf};
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Skill { pub name: String, pub purpose: String, pub effects: Vec<String>, pub status: String, pub body: String }

pub fn dir(root: &str, team: &str) -> PathBuf { crate::team_graph_run::teams_dir(root).join(crate::team_graph::slug(team)).join("skills") }
/// A skill's name is a plain word: letters, digits, `-` and `_`; anything else is folded to `-`.
pub fn clean(name: &str) -> String { name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-').to_string() }

/// THE SOURCE OF A SKILL IS YAML (operator 2026-11-25: "YAML could force the template, Markdown rendered from it"): `<name>.yaml` with the fixed fields
/// name, purpose, effects, status and playbook. What an agent reads is Markdown rendered from that (`to_markdown`). A `.md` written by an older run is still read.
pub fn to_yaml(s: &Skill) -> String {
    let mut h = yaml_rust2::yaml::Hash::new();
    h.insert(Yaml::String("name".into()), Yaml::String(s.name.clone()));
    h.insert(Yaml::String("purpose".into()), Yaml::String(s.purpose.replace('\n', " ")));
    h.insert(Yaml::String("effects".into()), Yaml::Array(s.effects.iter().map(|e| Yaml::String(e.clone())).collect()));
    h.insert(Yaml::String("status".into()), Yaml::String(s.status.clone()));
    h.insert(Yaml::String("playbook".into()), Yaml::String(s.body.trim_end().to_string()));
    let mut out = String::new(); let _ = YamlEmitter::new(&mut out).dump(&Yaml::Hash(h));
    out.trim_start_matches("---").trim_start().to_string() + "\n"
}
/// A skill from its YAML source. `None` when the text is not YAML. Missing fields take their defaults (a skill with no status is a draft).
pub fn from_yaml(text: &str, fallback: &str) -> Option<Skill> {
    let docs = YamlLoader::load_from_str(text).ok()?; let d = docs.first()?;
    let str_of = |k: &str| d[k].as_str().map(|x| x.trim().to_string()).unwrap_or_default();
    let effects: Vec<String> = d["effects"].as_vec().map(|v| v.iter().filter_map(|x| x.as_str().map(|e| e.trim().to_string())).filter(|e| !e.is_empty()).collect()).unwrap_or_default();
    let name = { let n = str_of("name"); if n.is_empty() { fallback.to_string() } else { n } };
    let status = { let st = str_of("status"); if st.is_empty() { "draft".to_string() } else { st } };
    Some(Skill { name, purpose: str_of("purpose"), effects, status, body: d["playbook"].as_str().unwrap_or("").trim_end().to_string() })
}
/// What an agent reads: the skill as Markdown, rendered from its fields.
pub fn to_markdown(s: &Skill) -> String {
    let effects = if s.effects.is_empty() { "none".to_string() } else { s.effects.join(", ") };
    format!("# {}\n\n{}\n\nEffects: {}\n\n{}\n", s.name, s.purpose, effects, s.body.trim_end())
}
pub fn parse(text: &str, fallback: &str) -> Skill {
    let mut sk = Skill { name: fallback.to_string(), purpose: String::new(), effects: vec![], status: "draft".into(), body: String::new() };
    let t = text.trim_start_matches('\u{feff}'); let Some(rest) = t.strip_prefix("---\n") else { sk.body = t.to_string(); return sk };
    let Some(end) = rest.find("\n---") else { sk.body = t.to_string(); return sk };
    for l in rest[..end].lines() { if let Some((k, v)) = l.split_once(':') { let v = v.trim(); match k.trim() { "name" if !v.is_empty() => sk.name = v.to_string(), "purpose" => sk.purpose = v.to_string(), "effects" => sk.effects = v.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(), "status" => sk.status = if v == "approved" { "approved".into() } else { "draft".into() }, _ => {} } } }
    sk.body = rest[end + 4..].trim_start_matches('\n').trim_end().to_string(); sk
}

fn read_one(d: &Path, n: &str) -> Option<Skill> {
    if let Ok(t) = std::fs::read_to_string(d.join(format!("{n}.yaml"))) { return from_yaml(&t, n); }
    std::fs::read_to_string(d.join(format!("{n}.md"))).ok().map(|t| parse(&t, n))
}
pub fn get(root: &str, team: &str, name: &str) -> Option<Skill> { read_one(&dir(root, team), &clean(name)) }
/// Save a skill as its YAML source (and drop an older Markdown copy of it, so there is one truth).
fn put(root: &str, team: &str, sk: &Skill) -> Result<(), String> {
    let d = dir(root, team); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join(format!("{}.yaml", sk.name)), to_yaml(sk)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(d.join(format!("{}.md", sk.name))); Ok(())
}
pub fn list(root: &str, team: &str) -> Vec<Skill> {
    let d = dir(root, team); let mut names: std::collections::BTreeSet<String> = Default::default();
    for e in std::fs::read_dir(&d).into_iter().flatten().flatten() { let p = e.path(); if matches!(p.extension().and_then(|x| x.to_str()), Some("yaml") | Some("md")) { if let Some(st) = p.file_stem() { names.insert(st.to_string_lossy().to_string()); } } }
    let mut v: Vec<Skill> = names.iter().filter_map(|n| read_one(&d, n)).collect();
    v.sort_by(|a, b| a.name.cmp(&b.name)); v
}

/// The Leader writes (or rewrites) a skill. It is a draft again whenever its words or declared effects change; an unchanged rewrite keeps the person's approval.
pub fn write(root: &str, team: &str, name: &str, purpose: &str, effects: &[String], body: &str) -> Result<Skill, String> {
    let n = clean(name); if n.is_empty() { return Err("a skill needs a name".into()); }
    if purpose.trim().is_empty() { return Err("a skill says what it is for (`purpose`)".into()); }
    if body.trim().is_empty() { return Err("a skill has a playbook: the steps an agent follows".into()); }
    if let Some(bad) = effects.iter().find(|e| !AGENT_EFFECTS.contains(&e.as_str())) { return Err(format!("effect `{bad}` is not one of {}", AGENT_EFFECTS.join(", "))); }
    let sk = Skill { name: n.clone(), purpose: purpose.trim().to_string(), effects: effects.to_vec(), status: "approved".into(), body: body.trim_end().to_string() };
    put(root, team, &sk)?; Ok(sk)
}
/// The person's word: the skill is as it was read. Never callable by an agent (the door refuses it).
pub fn approve(root: &str, team: &str, name: &str) -> Result<Skill, String> {
    let mut sk = get(root, team, name).ok_or_else(|| format!("there is no skill called {name}"))?; sk.status = "approved".into();
    put(root, team, &sk)?; Ok(sk)
}
pub fn remove(root: &str, team: &str, name: &str) -> bool { let n = clean(name); let d = dir(root, team); let a = std::fs::remove_file(d.join(format!("{n}.yaml"))).is_ok(); let b = std::fs::remove_file(d.join(format!("{n}.md"))).is_ok(); a || b }

/// What the skills of a graph's agents lack: a skill that is not there, one that declares an effect its agent does not have (a skill never widens an agent), and the drafts the person has yet to read. Pure over the folder.
pub struct SkillCheck { pub missing: Vec<(String, String)>, pub too_wide: Vec<(String, String, String)>, pub drafts: Vec<(String, String)> }
pub fn check(root: &str, g: &Graph) -> SkillCheck {
    let mut c = SkillCheck { missing: vec![], too_wide: vec![], drafts: vec![] };
    for (id, n) in &g.nodes { let Kind::Role { role: RoleKind::Agent, agent: Some(a), .. } = &n.kind else { continue };
        for name in &a.skills { match get(root, &g.team, name) {
            None => c.missing.push((id.clone(), name.clone())),
            Some(sk) => { if let Some(e) = sk.effects.iter().find(|e| !a.has(e)) { c.too_wide.push((id.clone(), sk.name.clone(), e.clone())); } if sk.status != "approved" { c.drafts.push((id.clone(), sk.name.clone())); } }
        } } }
    c
}
/// The graph's own validation plus what its skills lack. A draft is an error only for a RUN (`for_run`): a graph with drafts may be saved, shown and edited, never started.
pub fn report_for(root: &str, g: &Graph, for_run: bool) -> Report {
    let mut r = validate(g); let c = check(root, g);
    for (id, name) in c.missing { r.err("agent_skill_missing", Some(&id), format!("{id} uses the skill {name}, and there is none: the Leader writes it (antiloki_team_skill)")); }
    for (id, name, e) in c.too_wide { r.err("skill_exceeds_agent", Some(&id), format!("the skill {name} declares `{e}`, which {id} may not do: a skill never widens its agent")); }
    for (id, name) in c.drafts { if for_run { r.err("skill_unreviewed", Some(&id), format!("the skill {name} (used by {id}) is a draft: read it in the team pane and approve it first")); } else { r.warn("skill_unreviewed", Some(&id), format!("the skill {name} (used by {id}) is a draft until you approve it")); } }
    r
}
/// The playbooks an agent is given, in its prompt: each skill's purpose and steps. Blocking.
pub fn playbooks(root: &str, team: &str, names: &[String]) -> String {
    names.iter().filter_map(|n| get(root, team, n)).map(|s| to_markdown(&s)).collect::<Vec<_>>().join("\n")
}
pub fn exists(root: &str, team: &str) -> bool { Path::new(&dir(root, team)).is_dir() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::{AgentSpec, Node};
    fn tmp(n: &str) -> String { let p = std::env::temp_dir().join(format!("tskills-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() }
    fn graph_with(skills: &[&str], effects: &[&str]) -> Graph {
        let mut g = Graph { team: "Marketing".into(), schema: 1, branch: String::new(), budget_usd: 3.0, start: vec![], nodes: Default::default(), edges: vec![], triggers: vec![], limits: None, setpoints: Default::default() };
        let mut n = Node::role(RoleKind::Agent); if let Kind::Role { agent, .. } = &mut n.kind { *agent = Some(AgentSpec { title: "Copywriter".into(), responsibility: "write the posts".into(), brief: "write in our voice".into(), skills: skills.iter().map(|s| s.to_string()).collect(), effects: effects.iter().map(|s| s.to_string()).collect(), ..Default::default() }); }
        g.nodes.insert("copy".into(), n); g
    }
    #[test]
    fn a_skill_is_a_document_that_round_trips_and_the_leader_approves_what_it_writes() {
        let root = tmp("a"); let sk = write(&root, "Marketing", "Post Voice!", "how we sound", &["read".into()], "1. be plain\n2. be brief").unwrap();
        assert_eq!(sk.name, "post-voice"); assert_eq!(sk.status, "approved", "the Leader's skills are approved by default"); assert_eq!(get(&root, "Marketing", "post-voice").unwrap().body, "1. be plain\n2. be brief");
        // new words stay approved (the Leader wrote them); a skill the person marked draft by hand is read back as a draft and approve() still passes it
        write(&root, "Marketing", "post-voice", "how we sound", &["read".into()], "1. be loud").unwrap(); assert_eq!(get(&root, "Marketing", "post-voice").unwrap().status, "approved");
        let mut d = get(&root, "Marketing", "post-voice").unwrap(); d.status = "draft".into(); put(&root, "Marketing", &d).unwrap(); assert_eq!(get(&root, "Marketing", "post-voice").unwrap().status, "draft");
        approve(&root, "Marketing", "post-voice").unwrap(); assert_eq!(get(&root, "Marketing", "post-voice").unwrap().status, "approved");
        assert!(write(&root, "Marketing", "x", "p", &["destroy".into()], "b").is_err() && write(&root, "Marketing", "", "p", &[], "b").is_err() && write(&root, "Marketing", "x", "", &[], "b").is_err());
        assert_eq!(list(&root, "Marketing").len(), 1); assert!(remove(&root, "Marketing", "post-voice") && list(&root, "Marketing").is_empty());
    }
    #[test]
    fn a_skill_is_stored_as_yaml_with_fixed_fields_and_read_as_markdown_and_an_old_md_still_reads() {
        let root = tmp("yaml"); write(&root, "Marketing", "Brief", "write briefs", &["read".into()], "1. read the offer\n2. write it").unwrap();
        let d = dir(&root, "Marketing"); assert!(d.join("brief.yaml").is_file() && !d.join("brief.md").exists(), "one file, YAML");
        let y = std::fs::read_to_string(d.join("brief.yaml")).unwrap(); assert!(y.contains("purpose:") && y.contains("effects:") && y.contains("playbook:") && y.contains("status: approved"), "{y}");
        let md = to_markdown(&get(&root, "Marketing", "brief").unwrap()); assert!(md.starts_with("# brief") && md.contains("Effects: read") && md.contains("1. read the offer"), "{md}");
        assert!(playbooks(&root, "Marketing", &["brief".into()]).contains("write briefs"), "the agent's prompt carries the rendered playbook");
        // a skill written by an older run as Markdown is still read, and the next save makes it YAML
        std::fs::write(d.join("old.md"), "---\nname: old\npurpose: legacy\neffects: read\nstatus: approved\n---\nold steps\n").unwrap();
        assert_eq!(get(&root, "Marketing", "old").unwrap().body, "old steps"); approve(&root, "Marketing", "old").unwrap();
        assert!(d.join("old.yaml").is_file() && !d.join("old.md").exists(), "saving it moves it to YAML");
        // a broken or missing field gets the default: a skill with no status is a draft (it must be approved before a run)
        assert_eq!(from_yaml("name: x\npurpose: p\nplaybook: do it\n", "x").unwrap().status, "draft");
        assert_eq!(list(&root, "Marketing").len(), 2);
    }
    #[test]
    fn a_graph_cannot_run_on_a_missing_skill_a_draft_or_a_skill_wider_than_its_agent() {
        let root = tmp("b"); let g = graph_with(&["voice"], &["read"]);
        assert!(report_for(&root, &g, true).errors.iter().any(|e| e.code == "agent_skill_missing"));
        write(&root, "Marketing", "voice", "how we sound", &["read".into()], "be plain").unwrap();
        assert!(!report_for(&root, &g, true).errors.iter().any(|e| e.code == "skill_unreviewed"), "the Leader's skill is approved, so it does not stop a run");
        let mut d = get(&root, "Marketing", "voice").unwrap(); d.status = "draft".into(); put(&root, "Marketing", &d).unwrap();
        let r = report_for(&root, &g, true); assert!(r.errors.iter().any(|e| e.code == "skill_unreviewed"), "a draft stops a run");
        let r = report_for(&root, &g, false); assert!(!r.errors.iter().any(|e| e.code == "skill_unreviewed") && r.warnings.iter().any(|e| e.code == "skill_unreviewed"), "a draft only warns when the graph is merely saved or shown");
        approve(&root, "Marketing", "voice").unwrap(); assert!(!report_for(&root, &g, true).errors.iter().any(|e| e.code.starts_with("skill") || e.code.starts_with("agent_skill")));
        write(&root, "Marketing", "poster", "posts it", &["outward".into()], "post it").unwrap(); approve(&root, "Marketing", "poster").unwrap();
        let wide = graph_with(&["poster"], &["read"]); assert!(report_for(&root, &wide, true).errors.iter().any(|e| e.code == "skill_exceeds_agent"), "a skill never widens its agent");
    }
}
