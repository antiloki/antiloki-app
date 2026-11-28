//! WHAT EACH AGENT IS COSTING IN MEMORY, AND WHAT TO DO WHEN THE BOX RUNS OUT.
//!
//! operator 2026-09-24: *"be aware of RAM spent in each agent, show discretely at the agents the current ram used
//! in each one (live). So have a measure if the PC is close to froze like pause one process till other finishes
//! (only as emergency measure)."*
//!
//! An agent is not one process. `claude -p` spawns children, OpenCode runs a server, a build gate forks a whole
//! npm tree — so the number worth showing is the RSS of the process TREE under the pane's child, which is what
//! `tree_rss` walks. Everything that parses is pure and tested against real `/proc` text; everything that reads
//! the filesystem is a thin shell over it.
//!
//! The pause is deliberately hard to reach. Stopping a process mid-turn is a blunt instrument — it holds its file
//! handles, its sockets and whatever locks it took — so it happens only when the machine is genuinely about to
//! thrash, never to more than one agent at a time, never to the last one running, and it is announced in that
//! agent's own transcript rather than happening invisibly.
use serde::Serialize;

/// `VmRSS:\t  12668 kB` → bytes. Pure — tested.
pub fn rss_of(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}
/// The parent pid from `/proc/<pid>/stat`. The command name sits in parentheses and may itself contain spaces and
/// parentheses (`(my (weird) proc)`), so the fields are counted from the LAST `)` — splitting on whitespace from
/// the left is the classic way to read the wrong number here. Pure — tested.
pub fn ppid_of(stat: &str) -> Option<i32> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(1)?.parse().ok()
}
/// `MemTotal` and `MemAvailable`, in bytes. MemAvailable is the kernel's own estimate of what can be handed out
/// without swapping — the number to watch; MemFree is not, and treating it as one pauses agents on a healthy box.
/// Pure — tested.
pub fn meminfo(text: &str) -> Option<(u64, u64)> {
    let kb = |key: &str| -> Option<u64> {
        text.lines().find(|l| l.starts_with(key))?.split_whitespace().nth(1)?.parse::<u64>().ok().map(|v| v * 1024)
    };
    Some((kb("MemTotal:")?, kb("MemAvailable:")?))
}

/// How close the machine is to the wall. Pure — tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Pressure { Easy, Tight, Critical }
/// THE THRESHOLDS. A share of total, not an absolute: 700 MB free is comfortable on a 2 GB box and a crisis on a
/// 64 GB one. `Critical` is where a Linux desktop starts thrashing its page cache and the pointer stutters —
/// which is the "close to froze" the operator means, and it is deliberately lower than where a tuner would put a
/// warning, because the response to it is to stop somebody's work.
pub fn pressure_of(total: u64, available: u64) -> Pressure {
    if total == 0 { return Pressure::Easy }
    let share = available as f64 / total as f64;
    if share < 0.06 { Pressure::Critical } else if share < 0.14 { Pressure::Tight } else { Pressure::Easy }
}

/// `1.4 GB` · `812 MB` · `—` for nothing. Pure — tested.
pub fn human(bytes: u64) -> String {
    if bytes == 0 { return "—".into() }
    const G: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= G { format!("{:.1} GB", b / G) } else { format!("{:.0} MB", b / (1024.0 * 1024.0)) }
}

// ── reading the machine ──────────────────────────────────────────────────────────────────────────
//
// Three questions, two implementations: every live pid with its parent, one pid's resident memory, and the
// machine's total/available. Linux answers all three out of `/proc`. Windows has no `/proc` at all, so every
// one of these returned empty there — and because an unreadable machine is deliberately never an emergency
// (see `pressure_of`), the watchdog did not fail, it silently answered ZERO: every agent showed 0 B and the
// pressure never left Easy on the one platform the operator runs (found 2026-09-25, from
// `it_can_read_this_very_process` failing on Windows). The Windows side is raw kernel32, not a new
// dependency, for the same reason the `kill` extern below is.

#[cfg(unix)]
fn read(pid: i32, what: &str) -> Option<String> { std::fs::read_to_string(format!("/proc/{pid}/{what}")).ok() }

/// Every live pid with its parent — one pass over /proc, because walking it once per agent turns a 3-second
/// poll into a directory storm on a box with a few hundred processes.
#[cfg(unix)]
pub fn process_parents() -> Vec<(i32, i32)> {
    let Ok(rd) = std::fs::read_dir("/proc") else { return vec![] };
    let mut out = vec![];
    for e in rd.flatten() {
        let Ok(name) = e.file_name().into_string() else { continue };
        let Ok(pid) = name.parse::<i32>() else { continue };
        if let Some(stat) = read(pid, "stat") { if let Some(ppid) = ppid_of(&stat) { out.push((pid, ppid)); } }
    }
    out
}

/// One process's resident memory, or `None` when it is gone or the OS will not say. The platform seam
/// `tree_rss` is written over, so the walk itself stays pure and shared.
#[cfg(unix)]
fn rss_of_pid(pid: i32) -> Option<u64> { read(pid, "status").as_deref().and_then(rss_of) }

/// Every live pid with its parent, from one process snapshot.
#[cfg(windows)]
pub fn process_parents() -> Vec<(i32, i32)> { win::parents() }

/// One process's working set — the Windows counterpart of RSS.
#[cfg(windows)]
fn rss_of_pid(pid: i32) -> Option<u64> { win::rss(pid) }
/// The pids of a process and everything descended from it. Pure over the (pid, ppid) pairs — tested.
pub fn tree_of(root: i32, parents: &[(i32, i32)]) -> Vec<i32> {
    let mut out = vec![root];
    let mut i = 0;
    // breadth-first over the pairs; `i` never rewinds, so a cycle in a corrupt listing cannot spin forever
    while i < out.len() {
        let p = out[i]; i += 1;
        for (pid, ppid) in parents { if *ppid == p && !out.contains(pid) { out.push(*pid); } }
        if out.len() > 4096 { break }
    }
    out
}
/// The resident memory of a process tree, now. 0 when the process is gone.
pub fn tree_rss(root: i32, parents: &[(i32, i32)]) -> u64 {
    tree_of(root, parents).into_iter().filter_map(rss_of_pid).sum()
}
/// The machine's memory, now: total, and what can be handed out without swapping.
#[cfg(unix)]
pub fn system() -> Option<(u64, u64)> { meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?) }
#[cfg(windows)]
pub fn system() -> Option<(u64, u64)> { win::system() }

/// THE WINDOWS ANSWERS, in raw kernel32 — every symbol here lives in the library Rust already links, so this
/// costs no dependency and no build script. Handles are `isize` because that is `HANDLE`'s ABI on Windows;
/// the two failure sentinels differ and are not interchangeable: `CreateToolhelp32Snapshot` returns
/// `INVALID_HANDLE_VALUE` (-1) while `OpenProcess` returns NULL (0).
#[cfg(windows)]
mod win {
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    /// PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ — the pair `GetProcessMemoryInfo` documents as its
    /// minimum, and the pair a normal user holds over their own children. A process this user may not open (a
    /// service, another account's) simply has no number, exactly as an unreadable `/proc` entry has none.
    const ACCESS: u32 = 0x1000 | 0x0010;
    const INVALID_HANDLE: isize = -1;

    #[repr(C)]
    struct MemoryStatusEx {
        length: u32, memory_load: u32,
        total_phys: u64, avail_phys: u64,
        total_page: u64, avail_page: u64,
        total_virtual: u64, avail_virtual: u64, avail_extended_virtual: u64,
    }

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32, usage: u32, pid: u32, default_heap_id: usize, module_id: u32,
        threads: u32, parent_pid: u32, pri_class_base: i32, flags: u32, exe_file: [u16; 260],
    }

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32, page_fault_count: u32,
        peak_working_set: usize, working_set: usize,
        quota_peak_paged: usize, quota_paged: usize,
        quota_peak_nonpaged: usize, quota_nonpaged: usize,
        pagefile: usize, peak_pagefile: usize,
    }

    extern "system" {
        fn GlobalMemoryStatusEx(buf: *mut MemoryStatusEx) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Process32FirstW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }

    pub fn parents() -> Vec<(i32, i32)> {
        // SAFETY: the snapshot is closed on every path out; `size` is set before the first call because the
        // walk refuses to start without it (ERROR_INVALID_PARAMETER), which is the classic way to get an
        // empty process list here and read it as "the box has no processes".
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap == INVALID_HANDLE { return vec![] }
        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut out = vec![];
        let mut more = unsafe { Process32FirstW(snap, &mut entry) };
        while more != 0 {
            out.push((entry.pid as i32, entry.parent_pid as i32));
            more = unsafe { Process32NextW(snap, &mut entry) };
        }
        unsafe { CloseHandle(snap) };
        out
    }

    pub fn rss(pid: i32) -> Option<u64> {
        // SAFETY: NULL means the process is gone or not ours to open — no handle to close in that case.
        let h = unsafe { OpenProcess(ACCESS, 0, pid as u32) };
        if h == 0 { return None }
        let mut c: ProcessMemoryCounters = unsafe { std::mem::zeroed() };
        c.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
        let ok = unsafe { K32GetProcessMemoryInfo(h, &mut c, c.cb) };
        unsafe { CloseHandle(h) };
        (ok != 0).then_some(c.working_set as u64)
    }

    pub fn system() -> Option<(u64, u64)> {
        let mut m: MemoryStatusEx = unsafe { std::mem::zeroed() };
        m.length = std::mem::size_of::<MemoryStatusEx>() as u32;
        // AvailPhys is the analogue of MemAvailable, not MemFree: what can be handed out now.
        if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 { return None }
        Some((m.total_phys, m.avail_phys))
    }
}

// ── the emergency ────────────────────────────────────────────────────────────────────────────────
/// One agent as the watchdog sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Watched { pub pane_id: i64, pub pid: i32, pub rss: u64, pub busy: bool, pub paused: bool }

/// WHO GETS PAUSED — or nobody.
///
/// The rules, in order, and each one is there to stop a specific bad outcome:
/// · only under `Critical` — a tight box is not an emergency, and pausing on tight would pause constantly;
/// · never when only one agent is working, because pausing the only worker does not free the machine, it just
///   stops the machine doing anything while still holding the memory;
/// · never one already paused;
/// · the hungriest busy agent, since the point is to free memory and pausing a small one buys nothing.
/// Pure — tested.
pub fn pick_to_pause(p: Pressure, agents: &[Watched]) -> Option<i64> {
    if p != Pressure::Critical { return None }
    let busy: Vec<&Watched> = agents.iter().filter(|a| a.busy && !a.paused && a.pid > 0).collect();
    if busy.len() < 2 { return None }
    busy.iter().max_by_key(|a| a.rss).map(|a| a.pane_id)
}
/// WHO GETS RESUMED. Everything paused, the moment the machine is no longer critical — a paused agent is work the
/// operator asked for that is not happening, so it is resumed at the first opportunity rather than held until the
/// box is roomy. Pure — tested.
pub fn pick_to_resume(p: Pressure, agents: &[Watched]) -> Vec<i64> {
    if p == Pressure::Critical { return vec![] }
    agents.iter().filter(|a| a.paused).map(|a| a.pane_id).collect()
}

#[cfg(unix)]
fn signal(pid: i32, sig: i32) -> bool {
    extern "C" { fn kill(pid: i32, sig: i32) -> i32; }
    // the whole process GROUP: `claude -p` is a shell of children and stopping only the parent leaves the
    // memory exactly where it was, which is the one thing this must not do
    unsafe { kill(-pid, sig) == 0 || kill(pid, sig) == 0 }
}
/// WINDOWS has no SIGSTOP: `NtSuspendProcess` / `NtResumeProcess` (ntdll) freeze and thaw a whole process, the way the Task Manager's "Suspend" does. They act on ONE process, so the whole tree is walked
/// (a `claude -p` is a shell of children, and freezing only the parent leaves their memory where it was). A process this user may not open is skipped, exactly as an unreadable one has no memory figure.
#[cfg(windows)]
fn signal(pid: i32, sig: i32) -> bool {
    #[link(name = "kernel32")] extern "system" { fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize; fn CloseHandle(h: isize) -> i32; }
    #[link(name = "ntdll")] extern "system" { fn NtSuspendProcess(h: isize) -> i32; fn NtResumeProcess(h: isize) -> i32; }
    const PROCESS_SUSPEND_RESUME: u32 = 0x0800;
    let mut any = false;
    for p in tree_of(pid, &process_parents()) {
        // SAFETY: plain kernel32/ntdll calls on a handle this function opens and closes itself
        unsafe {
            let h = OpenProcess(PROCESS_SUSPEND_RESUME, 0, p as u32); if h == 0 { continue }
            let status = if sig == SIGSTOP { NtSuspendProcess(h) } else { NtResumeProcess(h) };
            CloseHandle(h); if status >= 0 { any = true }
        }
    }
    any
}
#[cfg(not(any(unix, windows)))]
fn signal(_pid: i32, _sig: i32) -> bool { false }

pub const SIGSTOP: i32 = 19;
pub const SIGCONT: i32 = 18;
pub fn pause_tree(pid: i32) -> bool { signal(pid, SIGSTOP) }
pub fn resume_tree(pid: i32) -> bool { signal(pid, SIGCONT) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proc_formats_are_read_the_way_the_kernel_writes_them() {
        assert_eq!(rss_of("Name:\tclaude\nVmRSS:\t   12668 kB\nThreads:\t9\n"), Some(12_668 * 1024));
        assert_eq!(rss_of("Name:\tx\nThreads:\t1\n"), None, "a kernel thread has no VmRSS and is not an error");

        assert_eq!(ppid_of("1607858 (cat) R 1607854 1607858 ..."), Some(1_607_854));
        // the comm field can hold spaces AND parentheses; counting from the left reads the wrong number
        assert_eq!(ppid_of("42 (my (weird) proc) S 7 42 ..."), Some(7));
        assert_eq!(ppid_of("garbage"), None);

        let mi = "MemTotal:       16169736 kB\nMemFree:          556680 kB\nBuffers:              44 kB\nMemAvailable:    4484188 kB\n";
        assert_eq!(meminfo(mi), Some((16_169_736 * 1024, 4_484_188 * 1024)));
        assert_eq!(meminfo("MemTotal: 1 kB"), None, "without MemAvailable there is no answer worth giving");
    }

    #[test]
    fn pressure_is_a_share_of_the_box_not_a_number_of_megabytes() {
        let gb = |n: f64| (n * 1024.0 * 1024.0 * 1024.0) as u64;
        assert_eq!(pressure_of(gb(16.0), gb(4.4)), Pressure::Easy);
        assert_eq!(pressure_of(gb(16.0), gb(2.0)), Pressure::Tight);
        assert_eq!(pressure_of(gb(16.0), gb(0.8)), Pressure::Critical);
        // 700 MB free is comfortable on a 2 GB box and a crisis on a 64 GB one — the same absolute, both answers
        assert_eq!(pressure_of(gb(2.0), gb(0.7)), Pressure::Easy);
        assert_eq!(pressure_of(gb(64.0), gb(0.7)), Pressure::Critical);
        assert_eq!(pressure_of(0, 0), Pressure::Easy, "an unreadable /proc never triggers the emergency");
    }

    #[test]
    fn a_tree_is_the_agent_not_just_its_first_process() {
        // claude(100) → node(200) → esbuild(300), and an unrelated process
        let parents = vec![(100, 1), (200, 100), (300, 200), (400, 1)];
        let mut t = tree_of(100, &parents); t.sort();
        assert_eq!(t, vec![100, 200, 300]);
        assert_eq!(tree_of(400, &parents), vec![400]);
        assert_eq!(tree_of(999, &parents), vec![999], "a process that is gone is its own empty tree, not a panic");
        // a corrupt listing that claims a process is its own ancestor must not spin
        assert_eq!(tree_of(1, &[(1, 2), (2, 1)]).len(), 2);
    }

    #[test]
    fn nobody_is_paused_unless_pausing_actually_helps() {
        let a = |id: i64, rss_gb: f64, busy: bool, paused: bool| Watched { pane_id: id, pid: id as i32 * 10, rss: (rss_gb * 1024.0 * 1024.0 * 1024.0) as u64, busy, paused };
        let two_busy = vec![a(1, 0.5, true, false), a(2, 2.0, true, false), a(3, 9.0, false, false)];

        // not an emergency: nothing happens, however hungry anybody is
        assert_eq!(pick_to_pause(Pressure::Easy, &two_busy), None);
        assert_eq!(pick_to_pause(Pressure::Tight, &two_busy), None, "tight is not an emergency — pausing here would never stop");

        // an emergency: the hungriest WORKING agent, not the hungriest agent
        assert_eq!(pick_to_pause(Pressure::Critical, &two_busy), Some(2), "the 9 GB one is idle — stopping it frees nothing that is being used");

        // the only worker is never paused: the memory stays held and nothing progresses
        assert_eq!(pick_to_pause(Pressure::Critical, &[a(1, 8.0, true, false), a(2, 1.0, false, false)]), None);
        // one already stopped does not get stopped again — and with it out of the running, agent 2 is now the
        // ONLY worker, so it is spared for the same reason as above: stopping it would stop everything
        assert_eq!(pick_to_pause(Pressure::Critical, &[a(1, 8.0, true, true), a(2, 1.0, true, false)]), None);
        // with two still running beside the paused one, the hungrier of the two goes
        assert_eq!(pick_to_pause(Pressure::Critical, &[a(1, 8.0, true, true), a(2, 1.0, true, false), a(3, 3.0, true, false)]), Some(3));
        assert_eq!(pick_to_pause(Pressure::Critical, &[]), None);

        // and everything paused comes back the moment it is no longer critical
        let paused = vec![a(1, 2.0, true, true), a(2, 1.0, true, true), a(3, 1.0, true, false)];
        assert_eq!(pick_to_resume(Pressure::Tight, &paused), vec![1, 2]);
        assert_eq!(pick_to_resume(Pressure::Easy, &paused), vec![1, 2]);
        assert_eq!(pick_to_resume(Pressure::Critical, &paused), Vec::<i64>::new(), "still critical — they wait");
    }

    #[test]
    fn the_number_reads_at_a_glance() {
        assert_eq!(human(0), "—");
        assert_eq!(human(812 * 1024 * 1024), "812 MB");
        assert_eq!(human(1024 * 1024 * 1024 + 1024 * 1024 * 400), "1.4 GB");
    }

    #[test]
    fn it_can_read_this_very_process() {
        // THE ONE TEST THAT READS THE REAL MACHINE, and so the only one that can tell the two platform
        // implementations apart. It failed on Windows until 2026-09-25 for the plainest possible reason:
        // there is no /proc there, so the watchdog answered zero for every agent instead of failing.
        let parents = process_parents();
        assert!(parents.len() > 1, "the OS should list more than one process, got {}", parents.len());
        let me = std::process::id() as i32;
        let rss = tree_rss(me, &parents);
        assert!(rss > 1024 * 1024, "the test process should hold at least a megabyte, got {rss}");
        // this process must appear in the listing, or `tree_of` has nothing to descend from
        assert!(parents.iter().any(|(pid, _)| *pid == me), "this process ({me}) is missing from the listing");
        let (total, avail) = system().expect("the machine's memory should be readable on this box");
        assert!(total > avail && avail > 0, "total {total}, available {avail}");
    }
}

/// CPU seconds (user + system) a process has used, from its `/proc/<pid>/stat` text. The fields after the `)` that closes the name: utime is the 12th, stime the 13th. Pure.
pub fn cpu_ticks(stat: &str) -> Option<u64> { let rest = stat.rsplit_once(')')?.1; let f: Vec<&str> = rest.split_whitespace().collect(); Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?) }
/// A model server is hogging the machine: the share of one core its processes used over a window (100 = one core), and whether that is a hog (over 250 for the window). Pure.
pub fn cpu_percent(ticks_before: u64, ticks_after: u64, secs: u64) -> u64 { if secs == 0 { 0 } else { (ticks_after.saturating_sub(ticks_before)) * 100 / (100 * secs) } }
/// Total CPU ticks of the local model server's processes (`ollama`, `llama-server`). Linux only; 0 elsewhere.
#[cfg(target_os = "linux")]
pub fn model_server_ticks() -> u64 {
    let mut t = 0; for d in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let name = d.file_name().to_string_lossy().to_string(); if !name.bytes().all(|b| b.is_ascii_digit()) { continue; }
        let Ok(comm) = std::fs::read_to_string(d.path().join("comm")) else { continue }; let c = comm.trim();
        if c.starts_with("ollama") || c.starts_with("llama-server") { if let Some(x) = std::fs::read_to_string(d.path().join("stat")).ok().and_then(|s| cpu_ticks(&s)) { t += x; } }
    } t
}
#[cfg(not(target_os = "linux"))]
pub fn model_server_ticks() -> u64 { 0 }
#[cfg(test)]
mod cpu_tests {
    use super::*;
    #[test] fn cpu_of_a_process_is_read_past_the_name_with_spaces() {
        let s = "123 (my (odd) name) S 1 2 3 4 5 6 7 8 9 10 400 150 0 0 20 0 1"; assert_eq!(cpu_ticks(s), Some(550)); assert_eq!(cpu_ticks("garbage"), None);
        assert_eq!(cpu_percent(0, 3200, 10), 320); assert_eq!(cpu_percent(5, 5, 10), 0); assert_eq!(cpu_percent(9, 1, 10), 0); assert_eq!(cpu_percent(0, 1, 0), 0);
    }
}

// THE MACHINE COMES FIRST (the operator 2026-11-22: "im trying to use some programs on my PC and they get closed abruptly because you take all the ram and cpu... antiloki should be aware of that and have this watcher with pause-continue switches automatically").
// Every background CLI run (a swarm or evolve experiment, a goal task, an X-ray call: all go through `cli::run_cli`) is registered here while it lives. `watch_headless` looks at the machine every few seconds and, when memory is short or the CPU is
// over-booked, PAUSES one run at a time (SIGSTOP to its group: its memory stays but it stops using the CPU, and nothing is lost), and CONTINUES them one at a time when there is room again. The person's own programs win. Panes (agents the person
// watches) keep their own watchdog in agent.rs.
static HEADLESS: std::sync::Mutex<Vec<(i32, bool)>> = std::sync::Mutex::new(Vec::new());
static HEADLESS_WHY: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// Registered while a background run lives; dropping it forgets the run (and lets it go if it was paused).
pub struct HeadlessGuard(i32);
impl Drop for HeadlessGuard { fn drop(&mut self) { let was = { let mut v = HEADLESS.lock().unwrap_or_else(|p| p.into_inner()); let w = v.iter().find(|(p, _)| *p == self.0).map(|x| x.1).unwrap_or(false); v.retain(|(p, _)| *p != self.0); w }; if was { resume_tree(self.0); } } }
pub fn headless_register(pid: i32) -> HeadlessGuard { HEADLESS.lock().unwrap_or_else(|p| p.into_inner()).push((pid, false)); HeadlessGuard(pid) }
/// Is this run stopped by the watcher right now?
pub fn headless_paused(pid: i32) -> bool { HEADLESS.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|x| x.0 == pid && x.1) }
/// A CALL'S TIME LIMIT COUNTS ONLY THE TIME IT COULD RUN (found 2026-11-27: a group member and a routine "timed out" while the watcher held them stopped on a busy machine). Ends after `limit` of running time, or six
/// times that on the clock; returns how long it was held. `held` says whether it is stopped now.
pub async fn run_time(limit: std::time::Duration, step: std::time::Duration, held: impl Fn() -> bool) -> std::time::Duration {
    let (mut ran, mut waited) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    while ran < limit && ran + waited < limit.saturating_mul(6) { let t = step.min(limit - ran); let t0 = tokio::time::Instant::now(); tokio::time::sleep(t).await; if held() { waited += t0.elapsed(); } else { ran += t0.elapsed(); } }
    waited
}
/// (running, paused, why the last pause happened) - for the screens.
/// What the Swarm panel shows about the machine: free and total memory in GB, the core count, the 1-minute load, and the two memory thresholds the watcher uses (GB). Pure over what it is given.
pub fn machine_view(total: u64, avail: u64, load: Option<f64>, cores: usize) -> serde_json::Value { let gb = |b: u64| (b as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0; const G: u64 = 1 << 30; serde_json::json!({ "free_gb": gb(avail), "total_gb": gb(total), "cores": cores, "load1": load, "pause_under_gb": gb((total / 100 * 15).max(5 * G / 2)), "continue_over_gb": gb((total / 100 * 25).max(4 * G)) }) }
pub fn machine_now() -> serde_json::Value { match system() { Some((t, a)) => machine_view(t, a, load1(), std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2)), None => serde_json::Value::Null } }
pub fn headless_status() -> (usize, usize, String) { let v = HEADLESS.lock().unwrap_or_else(|p| p.into_inner()); let paused = v.iter().filter(|x| x.1).count(); (v.len() - paused, paused, HEADLESS_WHY.lock().map(|s| s.clone()).unwrap_or_default()) }
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Switch { Pause, Resume, Hold }
/// The thresholds, in one place. Memory: pause under max(2.5 GiB, 15 %) available, continue over max(4 GiB, 25 %). CPU: pause when the 1-minute load is over the core count, continue under 60 % of it. The gap between each pair is what stops
/// a run from being paused and continued every few seconds.
pub fn switch_for(total: u64, avail: u64, load1: Option<f64>, cores: usize, running: usize, paused: usize) -> (Switch, String) {
    const GIB: u64 = 1 << 30; let low = (total / 100 * 15).max(5 * GIB / 2); let ok = (total / 100 * 25).max(4 * GIB); let c = cores.max(1) as f64;
    let short_mem = avail < low; let busy_cpu = load1.map_or(false, |l| l > c);
    // A BUSY PROCESSOR NEVER STOPS THE ONLY BACKGROUND RUN: it mostly waits on its provider, and stopping it frees nothing (2026-11-27)
    if (short_mem && running > 0) || (busy_cpu && running > 1) { return (Switch::Pause, if short_mem { format!("only {} of memory is free", human(avail)) } else { format!("the CPU is over-booked (load {:.1} on {} cores)", load1.unwrap_or(0.0), cores) }); }
    if paused > 0 && avail >= ok && (running == 0 || load1.map_or(true, |l| l < c * 0.6)) { return (Switch::Resume, String::new()); }
    (Switch::Hold, String::new())
}
pub fn load1() -> Option<f64> { std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok() }
/// One look at the machine and at most one switch. Returns what it did (for a test and a log).
pub fn headless_tick(total: u64, avail: u64, load: Option<f64>, cores: usize) -> Switch {
    let (running, paused) = { let v = HEADLESS.lock().unwrap_or_else(|p| p.into_inner()); let p = v.iter().filter(|x| x.1).count(); (v.len() - p, p) };
    let (sw, why) = switch_for(total, avail, load, cores, running, paused);
    let mut v = HEADLESS.lock().unwrap_or_else(|p| p.into_inner());
    match sw {
        Switch::Pause => { if let Some(e) = v.iter_mut().rev().find(|x| !x.1) { if pause_tree(e.0) { e.1 = true; *HEADLESS_WHY.lock().unwrap_or_else(|p| p.into_inner()) = why; } } }
        Switch::Resume => { if let Some(e) = v.iter_mut().find(|x| x.1) { resume_tree(e.0); e.1 = false; if v.iter().all(|x| !x.1) { HEADLESS_WHY.lock().unwrap_or_else(|p| p.into_inner()).clear(); } } }
        Switch::Hold => {}
    } sw
}
/// The watcher: every 3 s, on its own thread so a busy runtime cannot delay it.
pub fn watch_headless() {
    std::thread::spawn(|| loop { std::thread::sleep(std::time::Duration::from_secs(3)); if let Some((t, a)) = system() { headless_tick(t, a, load1(), std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2)); } });
}
#[cfg(test)]
mod machine_tests {
    use super::*;
    const G: u64 = 1 << 30;
    #[test] fn the_switch_pauses_when_memory_or_cpu_is_short_and_continues_only_with_room_to_spare() {
        let t = 16 * G;
        assert_eq!(switch_for(t, 2 * G, Some(0.5), 8, 2, 0).0, Switch::Pause); assert!(switch_for(t, 2 * G, None, 8, 2, 0).1.contains("memory"));
        assert_eq!(switch_for(t, 10 * G, Some(9.5), 8, 2, 0).0, Switch::Pause); assert!(switch_for(t, 10 * G, Some(9.5), 8, 2, 0).1.contains("CPU"));
        assert_eq!(switch_for(t, 10 * G, Some(2.0), 8, 2, 0).0, Switch::Hold, "plenty of room: nothing to do");
        assert_eq!(switch_for(t, 3 * G, Some(1.0), 8, 1, 1).0, Switch::Hold, "between the two thresholds a paused run stays paused: no blinking every few seconds");
        assert_eq!(switch_for(t, 8 * G, Some(6.0), 8, 1, 1).0, Switch::Hold, "load still high");
        assert_eq!(switch_for(t, 10 * G, Some(20.0), 8, 1, 0).0, Switch::Hold, "the only run is not stopped for a busy processor"); assert_eq!(switch_for(t, 8 * G, Some(20.0), 8, 0, 1).0, Switch::Resume, "nor left stopped when nothing else runs");
        assert_eq!(switch_for(t, 8 * G, Some(3.0), 8, 0, 1).0, Switch::Resume);
        assert_eq!(switch_for(t, 1 * G, Some(20.0), 8, 0, 0).0, Switch::Hold, "nothing running, nothing to pause");
    }
    #[tokio::test] async fn time_held_stopped_does_not_count_against_a_calls_limit() {
        use std::time::Duration; let ms = Duration::from_millis;
        let n = std::sync::atomic::AtomicUsize::new(0); let held = || n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 3;
        let t0 = std::time::Instant::now(); let waited = run_time(ms(60), ms(20), held).await; assert!(waited >= ms(55) && t0.elapsed() >= ms(115), "{waited:?}");
        let t0 = std::time::Instant::now(); run_time(ms(30), ms(10), || true).await; assert!(t0.elapsed() >= ms(170) && t0.elapsed() < ms(2_000), "a call held for ever still ends");
    }
    #[test] fn the_machine_view_names_free_memory_cores_and_the_two_thresholds() {
        let v = machine_view(16 * G, 10 * G + G / 2, Some(1.25), 8); assert_eq!(v["free_gb"], 10.5); assert_eq!(v["total_gb"], 16.0); assert_eq!(v["cores"], 8); assert_eq!(v["pause_under_gb"], 2.5); assert_eq!(v["continue_over_gb"], 4.0);
        let big = machine_view(64 * G, 30 * G, None, 16); assert_eq!(big["pause_under_gb"], 9.6); assert_eq!(big["continue_over_gb"], 16.0); assert!(big["load1"].is_null());
    }
    #[cfg(unix)] #[test] fn a_real_process_is_stopped_then_continued_and_forgotten_when_its_guard_drops() {
        let mut c = std::process::Command::new("sleep").arg("30").spawn().unwrap(); let pid = c.id() as i32; let g = headless_register(pid);
        assert_eq!(headless_tick(16 * G, 1 * G, Some(0.1), 8), Switch::Pause); assert!(headless_status().1 >= 1);
        let state = |p: i32| std::fs::read_to_string(format!("/proc/{p}/stat")).unwrap().rsplit_once(')').unwrap().1.trim().chars().next().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100)); assert_eq!(state(pid), 'T', "stopped");
        assert_eq!(headless_tick(16 * G, 12 * G, Some(0.1), 8), Switch::Resume); std::thread::sleep(std::time::Duration::from_millis(100)); assert_ne!(state(pid), 'T', "continued");
        drop(g); let _ = c.kill(); let _ = c.wait();
    }
}
