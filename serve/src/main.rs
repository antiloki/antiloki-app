//! Standalone dev/parity binary. The desktop app does NOT use this — it links the library and
//! calls `run()` in-process, which is what makes the shipped artifact a single executable.
//!
//!   antiloki-serve            serve on the configured port
//!   antiloki-serve doctor     print what this machine has and exit (PLAN-THE-LEAN-YEARS I.6)

/// THE RUNTIME'S THREADS GET A BIG STACK (2026-09-29): the operator opened a second, large workspace in the browser and the serve died with
/// "thread 'tokio-rt-worker' has overflowed its stack" right after the workspace switch (the debug build's frames, walking a big tree). tokio's default is 2 MB per
/// thread; 64 MB is address space, committed only if used, and it applies to the blocking pool too.
fn main() {
    // THE TOOL FOLDERS ARE ON THE SERVE'S PATH (operator 2026-11-10: "i see only 2 providers installed ... release all installed on this machine"). Started from a launcher, a service or a bare terminal, the serve's PATH may
    // lack the folders the agent CLIs and the project's own tools live in (mise's shims, ~/.local/bin, cargo, volta, Homebrew): Cursor and Antigravity were "not installed" and `npm` was "not found". Added once, before
    // anything starts (and only what is missing: a PATH that has them is left exactly as it was), so detection, every terminal and every dev script inherit them.
    { let cur = std::env::var("PATH").unwrap_or_default(); let ext = antiloki_rs::proc::extend_path(&cur, &antiloki_rs::proc::tool_path_extras()); if ext != cur { std::env::set_var("PATH", ext); } }
    antiloki_rs::proc::import_session_env();
    // NOTHING ANTILOKI STARTS OPENS A BROWSER BY ITSELF (operator 2026-11-26: "it is opening the login of cursor all the time"). Cursor honours this; a sign-in the person starts in a pane prints its address there.
    if std::env::var_os("NO_OPEN_BROWSER").is_none() { std::env::set_var("NO_OPEN_BROWSER", "1"); }
    tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(64 * 1024 * 1024).build().expect("the async runtime")
        .block_on(async_main());
}

async fn async_main() {
    // DOCTOR MUST NOT NEED A RUNNING SERVE. The first thing anyone reaches for when something is wrong is the
    // thing that is broken; a report that only works when the server starts answers nothing on the machine
    // that needs it. Everything it reads is the filesystem and the record, so it runs before `run()` ever does.
    if std::env::args().nth(1).as_deref() == Some("doctor") {
        let root = std::env::args().nth(2)
            .or_else(|| std::env::var("ANTILOKI_WORKSPACE").ok())
            .unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
        println!("{}", antiloki_serve::doctor::render(&antiloki_serve::doctor::report(&root)));
        return;
    }
    // A CLEAN STOP ENDS ITS DEV SERVERS (2026-09-28). Only here: the desktop app links `run()` in-process and owns its own exit,
    // and a crash or a SIGKILL is covered by the sweep the next start does.
    #[cfg(unix)]
    tokio::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else { return };
        tokio::select! { _ = term.recv() => {}, _ = int.recv() => {} }
        antiloki_serve::preview::stop_all_global();
        std::process::exit(0);
    });
    antiloki_serve::run(None).await;
}
