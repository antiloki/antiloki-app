//! Port of the BE's driver tick loop.
//!
//! This replaced an external driver-daemon process, and it is the thing that actually advances
//! the pipeline: each tick signs a call with the driver token and dispatches `system.driver.tick`.
//!
//! Three properties are load-bearing and were learned the hard way in the BE — the comments there
//! record the incidents, so they are preserved rather than re-derived:
//!
//!   * **Single-flight, self-scheduling.** The next tick is armed only after the current one has
//!     fully settled. `setInterval` fired every 2s regardless of in-flight work, so when a tick's
//!     AI call outran the interval, concurrent ticks raced on the SAME card (driver-schedule has
//!     no row lock) — double-dispatch and SQLITE_BUSY, whose swallowed errors became error
//!     notifications that halted the daemon.
//!   * **Dormancy, not polling.** A tick that advanced a card chains in 150ms so the card flows
//!     through its remaining stages in one burst; a tick with nothing to do arms NO timer and
//!     sleeps until `wake()`. The driver does not poll on a fixed interval.
//!   * **`failed` is not idle.** driver-tick swallows the per-card error and still writes
//!     driver_ticks + a notification + driver_decisions — all card-scoped state the waterfall
//!     terminal must reflect. Pinging only on success left the terminal static. The CHAIN
//!     condition stays didWork-only, though: a stuck card must not retry on a timer.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::dispatch::AppState;

fn env_ms(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn halt_file() -> PathBuf {
    let home = std::env::var("ANTILOKI_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let mut p = dirs_home();
        p.push(".antiloki");
        p
    });
    home.join("run").join("driver-halt")
}

fn dirs_home() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub struct Ticker {
    state: Arc<AppState>,
    running: AtomicBool,
    wake_requested: AtomicBool,
    /// Set when no timer is armed, so wake() knows whether it must re-arm one.
    dormant: AtomicBool,
    halt_logged_at: Mutex<Option<std::time::Instant>>,
    /// IDLE BACK-OFF (2026-09-19): an idle tick every 2 s cost ~50% of a core on a big workspace (the
    /// driver reads the whole board each time). Consecutive idle ticks double the delay, 2 s → 60 s;
    /// any dispatch wakes the loop at once (`wake`), so a card that becomes actionable never waits.
    idle_streak: std::sync::atomic::AtomicU32,
}

impl Ticker {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            running: AtomicBool::new(false),
            wake_requested: AtomicBool::new(false),
            dormant: AtomicBool::new(true),
            halt_logged_at: Mutex::new(None),
            idle_streak: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// ANTILOKI_BE_TICK_DISABLED=true keeps the surface up with no pipeline motion — the same
    /// switch the BE's claude-test lane uses so an automated probe cannot advance real cards.
    pub fn enabled() -> bool {
        std::env::var("ANTILOKI_BE_TICK_DISABLED").as_deref() != Ok("true")
    }

    pub fn start(self: &Arc<Self>) {
        if !Self::enabled() {
            println!("antiloki-serve: tick loop DISABLED (ANTILOKI_BE_TICK_DISABLED=true)");
            return;
        }
        println!(
            "antiloki-serve: tick loop starting (every {}ms; halt file: {})",
            env_ms("ANTILOKI_BE_TICK_INTERVAL_MS", 2000),
            halt_file().display()
        );
        self.arm(Duration::from_millis(env_ms("ANTILOKI_BE_TICK_INTERVAL_MS", 2000)));
    }

    fn arm(self: &Arc<Self>, delay: Duration) {
        self.dormant.store(false, Ordering::SeqCst);
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            me.tick().await;
        });
    }

    /// Pull the next tick forward instead of waiting out dormancy. Called after an operator
    /// dispatch mutated state, so a card that just became actionable advances immediately.
    pub fn wake(self: &Arc<Self>) {
        self.wake_requested.store(true, Ordering::SeqCst);
        // Only re-arm if nothing is pending; a running tick re-arms itself on settle.
        if self.dormant.swap(false, Ordering::SeqCst) && !self.running.load(Ordering::SeqCst) {
            self.arm(Duration::from_millis(env_ms("ANTILOKI_BE_TICK_CHAIN_DELAY_MS", 150)));
        }
    }

    fn go_dormant(&self) {
        self.dormant.store(true, Ordering::SeqCst);
    }

    async fn tick(self: &Arc<Self>) {
        // Single-flight guard: a wake-scheduled timer can fire in the same instant a previous
        // tick started. Drop this firing; the running tick always re-arms on settle.
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.wake_requested.store(false, Ordering::SeqCst);

        if halt_file().exists() {
            let mut last = self.halt_logged_at.lock().unwrap();
            let now = std::time::Instant::now();
            if last.map(|t| now.duration_since(t).as_secs() >= 60).unwrap_or(true) {
                println!("antiloki-serve: tick loop paused: halt file present");
                *last = Some(now);
            }
            drop(last);
            self.running.store(false, Ordering::SeqCst);
            self.arm(Duration::from_millis(env_ms("ANTILOKI_BE_TICK_INTERVAL_MS", 2000)));
            return;
        }

        let conn = self.state.conn.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let token = antiloki_rs::audit_driver_token::loadOrCreateDriverToken()?;
            let ts = antiloki_rs::clock::now_iso_nodb();
            let signed = antiloki_rs::audit_driver_token::signDriverCall(&json!({
                "action": "system.driver.tick",
                "input": {},
                "ts": ts,
                "token": token,
                "actor": "driver:be",
            }));
            let ctx = json!({
                "actor": "driver:be",
                "source": "driver",
                "driver_hmac": signed.get("hmac").cloned().unwrap_or(Value::Null),
                "driver_ts": signed.get("ts").cloned().unwrap_or(Value::Null),
            });
            antiloki_rs::engine_action_run::skillRunWithContext(
                &json!({ "action": "system.driver.tick", "input": {} }),
                &guard,
                &ctx,
            )
        })
        .await;

        let (ok, outcome, card_slug) = match outcome {
            Ok(Ok(v)) => {
                let o = v.get("outcome").and_then(|x| x.as_str()).map(String::from);
                let slug = v.get("card_slug").and_then(|x| x.as_str()).map(String::from);
                if let Some(oc) = &o {
                    if oc != "idle" {
                        let action = v.get("decided_action").and_then(|x| x.as_str()).unwrap_or("(none)");
                        let rule = v
                            .get("decided_reason")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.split(':').next())
                            .unwrap_or("?");
                        println!("[tick] {} {rule} -> {action} = {oc}", slug.clone().unwrap_or_else(|| "?".into()));
                    }
                }
                (true, o, slug)
            }
            Ok(Err(e)) => {
                println!("antiloki-serve: tick failed (continuing): {e}");
                (false, None, None)
            }
            Err(join) => {
                println!("antiloki-serve: tick panicked (continuing): {join}");
                (false, None, None)
            }
        };

        self.running.store(false, Ordering::SeqCst);

        if ok {
            let did_work = matches!(outcome.as_deref(), Some("fired") | Some("escalated"));
            if did_work || outcome.as_deref() == Some("failed") {
                let mut topics = vec!["driver".to_string()];
                let mut entities = vec![("driver".to_string(), None, None)];
                if let Some(s) = &card_slug {
                    topics.push(format!("card:{s}"));
                    entities.push(("card".to_string(), Some(s.clone()), Some("update".to_string())));
                }
                self.state.events.emit(None, outcome.clone(), topics, entities);
            }
            if did_work || self.wake_requested.load(Ordering::SeqCst) {
                self.idle_streak.store(0, Ordering::SeqCst);
                self.arm(Duration::from_millis(env_ms("ANTILOKI_BE_TICK_CHAIN_DELAY_MS", 150)));
            } else {
                // idle: sleep longer each time (2 s → 60 s) instead of going fully dormant, so a card
                // that becomes actionable without a dispatch (a timer, an agent) is still picked up
                let n = self.idle_streak.fetch_add(1, Ordering::SeqCst).min(5);
                let base = env_ms("ANTILOKI_BE_TICK_INTERVAL_MS", 2000);
                self.arm(Duration::from_millis((base << n).min(60_000)));
            }
        } else {
            // A tick threw. The tick itself already wrote a notification (which pauses the loop);
            // do NOT retry on a timer — the operator resolving it is itself a dispatch that wakes us.
            self.state
                .events
                .emit(None, Some("failed".into()), vec!["driver".into()], vec![("driver".into(), None, None)]);
            self.go_dormant();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // The tick loop reads process-global env (ANTILOKI_HOME for the halt file and the driver token,
    // ANTILOKI_BE_TICK_* for the timing) and touches shared files under that home. Every test here holds
    // this lock, and ANTILOKI_HOME is pointed ONCE at a throwaway dir for the rest of the test process: the
    // real ~/.antiloki (the operator's driver token, halt file) is never read.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn home() -> PathBuf {
        static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let d = std::env::temp_dir().join(format!("antiloki-tick-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            std::env::set_var("ANTILOKI_HOME", &d);
            d
        }).clone()
    }
    struct EnvSnap(Vec<(String, Option<String>)>);
    impl EnvSnap { fn take(keys: &[&str]) -> EnvSnap { EnvSnap(keys.iter().map(|k| (k.to_string(), std::env::var(k).ok())).collect()) } }
    impl Drop for EnvSnap { fn drop(&mut self) { for (k, v) in &self.0 { match v { Some(v) => std::env::set_var(k, v), None => std::env::remove_var(k) } } } }

    fn ticker() -> Arc<Ticker> {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let home = home();
        let db_dir = home.join(format!("db-{}", N.fetch_add(1, Ordering::SeqCst))).join(".antiloki").join("db");
        std::fs::create_dir_all(&db_dir).unwrap();
        let conn = antiloki_rs::engine_open_db::openDb(&json!(db_dir.join("state.sqlite").to_string_lossy()), &json!({ "readOnly": false })).expect("a fresh substrate");
        Ticker::new(Arc::new(AppState { waker: std::sync::OnceLock::new(), events: crate::events::EventsHub::new(), conn: Arc::new(Mutex::new(conn)), pool: vec![], db_path: ":memory:".into() }))
    }

    /// no halt file, a usable auth dir: the state a healthy install is in
    fn healthy() {
        let h = home();
        let _ = std::fs::remove_file(halt_file());
        let auth = h.join("auth");
        if auth.is_file() { std::fs::remove_file(&auth).unwrap(); }
    }
    /// a huge interval keeps the timer a tick arms from firing inside the test
    fn quiet_timers() -> EnvSnap {
        let s = EnvSnap::take(&["ANTILOKI_BE_TICK_INTERVAL_MS", "ANTILOKI_BE_TICK_CHAIN_DELAY_MS", "ANTILOKI_BE_TICK_DISABLED"]);
        std::env::set_var("ANTILOKI_BE_TICK_INTERVAL_MS", "3600000");
        std::env::set_var("ANTILOKI_BE_TICK_CHAIN_DELAY_MS", "3600000");
        std::env::remove_var("ANTILOKI_BE_TICK_DISABLED");
        s
    }

    #[test]
    fn a_timing_env_var_is_a_number_of_milliseconds_or_the_default() {
        std::env::set_var("ANTILOKI_TICK_TEST_MS_A", "250");
        assert_eq!(env_ms("ANTILOKI_TICK_TEST_MS_A", 2000), 250);
        std::env::set_var("ANTILOKI_TICK_TEST_MS_A", "0");
        assert_eq!(env_ms("ANTILOKI_TICK_TEST_MS_A", 2000), 0, "zero is a legal value, not 'unset'");
        for bad in ["", "abc", "-5", "1.5", " 7"] {
            std::env::set_var("ANTILOKI_TICK_TEST_MS_A", bad);
            assert_eq!(env_ms("ANTILOKI_TICK_TEST_MS_A", 2000), 2000, "{bad:?}");
        }
        std::env::remove_var("ANTILOKI_TICK_TEST_MS_A");
        assert_eq!(env_ms("ANTILOKI_TICK_TEST_MS_A", 150), 150, "unset → the default");
    }

    #[tokio::test]
    async fn only_the_exact_word_true_switches_the_loop_off() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        assert!(Ticker::enabled());
        for (v, want) in [("true", false), ("TRUE", true), ("1", true), ("false", true), ("", true), (" true", true)] {
            std::env::set_var("ANTILOKI_BE_TICK_DISABLED", v);
            assert_eq!(Ticker::enabled(), want, "ANTILOKI_BE_TICK_DISABLED={v:?}");
        }
    }

    #[tokio::test]
    async fn a_disabled_loop_never_arms_a_timer() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        std::env::set_var("ANTILOKI_BE_TICK_DISABLED", "true");
        let t = ticker();
        t.start();
        assert!(t.dormant.load(Ordering::SeqCst), "start() on a disabled loop leaves it dormant");
        // and an enabled one arms
        std::env::remove_var("ANTILOKI_BE_TICK_DISABLED");
        t.start();
        assert!(!t.dormant.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn the_halt_file_lives_under_the_home_run_dir() {
        let _g = SERIAL.lock().await;
        assert_eq!(halt_file(), home().join("run").join("driver-halt"));
    }

    /// RULE (dormancy): a wake pulls the next tick forward ONLY when nothing is armed, and always leaves a note for a running tick.
    #[tokio::test]
    async fn a_wake_arms_a_dormant_loop_once_and_only_leaves_a_note_for_a_running_one() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        let t = ticker();
        assert!(t.dormant.load(Ordering::SeqCst), "a new ticker starts dormant");
        t.wake();
        assert!(t.wake_requested.load(Ordering::SeqCst));
        assert!(!t.dormant.load(Ordering::SeqCst), "the wake armed a timer");
        // a second wake while a timer is pending must not arm a second one: dormant is already false, nothing changes
        t.wake();
        assert!(!t.dormant.load(Ordering::SeqCst));
        // a wake during a running tick clears dormancy but arms nothing - the running tick re-arms itself on settle
        let running = ticker();
        running.running.store(true, Ordering::SeqCst);
        running.wake();
        assert!(running.wake_requested.load(Ordering::SeqCst) && !running.dormant.load(Ordering::SeqCst));
    }

    /// RULE (single flight): a tick that fires while another is running is dropped, touching nothing.
    #[tokio::test]
    async fn a_tick_that_fires_while_another_is_running_is_dropped() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        healthy();
        let t = ticker();
        let mut rx = t.state.events.subscribe();
        t.running.store(true, Ordering::SeqCst);
        t.wake_requested.store(true, Ordering::SeqCst);
        t.tick().await;
        assert!(t.running.load(Ordering::SeqCst), "the running tick's flag is not cleared by the dropped one");
        assert!(t.wake_requested.load(Ordering::SeqCst), "and its pending wake is not consumed");
        assert_eq!(t.idle_streak.load(Ordering::SeqCst), 0);
        assert!(rx.try_recv().is_err());
    }

    /// RULE: the operator's halt file pauses the loop - no dispatch, no event, the timer is still re-armed - and logs at most once a minute.
    #[tokio::test]
    async fn a_halt_file_pauses_the_pipeline_quietly_and_the_log_line_is_throttled() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        healthy();
        std::fs::create_dir_all(halt_file().parent().unwrap()).unwrap();
        std::fs::write(halt_file(), "stop").unwrap();
        let t = ticker();
        let mut rx = t.state.events.subscribe();
        t.tick().await;
        assert!(!t.running.load(Ordering::SeqCst), "the running flag is released");
        assert!(!t.dormant.load(Ordering::SeqCst), "a paused loop keeps a timer so the resume is noticed");
        assert_eq!(t.idle_streak.load(Ordering::SeqCst), 0, "a pause is not an idle tick");
        let first = t.halt_logged_at.lock().unwrap().expect("logged the pause");
        t.tick().await;
        assert_eq!(*t.halt_logged_at.lock().unwrap(), Some(first), "a second pause inside the minute does not log again");
        *t.halt_logged_at.lock().unwrap() = std::time::Instant::now().checked_sub(Duration::from_secs(61));
        let stale = *t.halt_logged_at.lock().unwrap();
        t.tick().await;
        assert!(t.halt_logged_at.lock().unwrap().unwrap() > stale.unwrap(), "after a minute it says so again");
        assert!(rx.try_recv().is_err(), "no event: nothing changed");
        // the file is the whole switch: remove it and the next tick runs
        std::fs::remove_file(halt_file()).unwrap();
        t.tick().await;
        assert_eq!(t.idle_streak.load(Ordering::SeqCst), 1, "an empty board is an idle tick");
    }

    /// RULE (idle back-off): a tick with nothing to do counts toward a longer wait and announces nothing; a wake resets the count.
    #[tokio::test]
    async fn idle_ticks_back_off_announce_nothing_and_a_wake_resets_the_count() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        healthy();
        let t = ticker();
        let mut rx = t.state.events.subscribe();
        for n in 1..=3u32 {
            t.tick().await;
            assert_eq!(t.idle_streak.load(Ordering::SeqCst), n, "each idle tick lengthens the next wait");
            assert!(!t.dormant.load(Ordering::SeqCst), "idle is not dormant: a card that becomes actionable on its own is still picked up");
            assert!(!t.running.load(Ordering::SeqCst));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(rx.try_recv().is_err(), "an idle tick sends no change to the FE");
        // an operator dispatch (wake) that lands WHILE a tick is in flight is seen when it settles: the back-off restarts and
        // the next tick is chained (150 ms) instead of waiting out the long idle wait. The tick is held on the db lock so
        // the wake can be delivered after it began (it clears the request at its start).
        let conn = t.state.conn.clone();
        let guard = conn.lock().unwrap();
        let t2 = t.clone();
        let flight = tokio::spawn(async move { t2.tick().await });
        for _ in 0..500 { if t.running.load(Ordering::SeqCst) { break; } tokio::time::sleep(Duration::from_millis(5)).await; }
        assert!(t.running.load(Ordering::SeqCst), "the tick began and is waiting for the connection");
        t.wake();
        assert!(t.wake_requested.load(Ordering::SeqCst));
        drop(guard);
        flight.await.unwrap();
        assert_eq!(t.idle_streak.load(Ordering::SeqCst), 0, "the wake restarted the back-off");
        assert!(!t.dormant.load(Ordering::SeqCst) && !t.running.load(Ordering::SeqCst));
        assert!(rx.try_recv().is_err(), "an idle outcome still announces nothing");
    }

    /// RULE: the wait grows 2 s → 60 s and stops growing (base << min(streak,5), capped at a minute) - measured with a small base.
    #[tokio::test]
    async fn the_idle_wait_doubles_and_the_loop_keeps_ticking_on_its_own() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        healthy();
        std::env::set_var("ANTILOKI_BE_TICK_INTERVAL_MS", "20");
        let t = ticker();
        t.start();
        tokio::time::sleep(Duration::from_millis(900)).await;
        let n = t.idle_streak.load(Ordering::SeqCst);
        // waits of 20, 40, 80, 160, 320 ms add up to 620 ms; a fixed 20 ms interval would have ticked ~40 times
        assert!((2..=8).contains(&n), "{n} idle ticks in 0.9 s at a 20 ms base: the wait must be doubling");
        t.state.events.subscribe(); // (nothing to hear: idle)
        std::env::set_var("ANTILOKI_BE_TICK_INTERVAL_MS", "3600000"); // park whatever timer comes next
    }

    /// RULE: a tick that throws is announced (so the FE refetches the notification it wrote), the loop stops polling on a timer,
    /// and the operator's next dispatch (wake) starts it again.
    #[tokio::test]
    async fn a_failed_tick_is_announced_goes_dormant_and_a_wake_tries_again() {
        let _g = SERIAL.lock().await;
        let _snap = quiet_timers();
        healthy();
        // no driver token can be created: `auth` is a file where the token directory has to be
        let auth = home().join("auth");
        let _ = std::fs::remove_dir_all(&auth);
        std::fs::write(&auth, "not a directory").unwrap();
        let t = ticker();
        let mut rx = t.state.events.subscribe();
        t.tick().await;
        assert!(t.dormant.load(Ordering::SeqCst), "no retry on a timer: a stuck tick must not spin");
        assert!(!t.running.load(Ordering::SeqCst));
        assert_eq!(t.idle_streak.load(Ordering::SeqCst), 0, "a failure is not an idle tick");
        let c = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.expect("the failure is announced").unwrap();
        assert_eq!((c["outcome"].as_str(), c["topics"].clone()), (Some("failed"), json!(["driver"])));
        assert_eq!(c["entities"], json!([{ "kind": "driver" }]));
        // the operator resolves it (a dispatch → wake): the loop is armed again and, if it fails again, says so again
        std::env::set_var("ANTILOKI_BE_TICK_CHAIN_DELAY_MS", "20");
        t.wake();
        let c2 = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.expect("the retry ran and failed again").unwrap();
        assert_eq!(c2["outcome"], "failed");
        assert!(t.dormant.load(Ordering::SeqCst));
        // and once the cause is gone, a wake brings the loop back to a healthy idle
        std::fs::remove_file(&auth).unwrap();
        t.wake();
        for _ in 0..100 { if t.idle_streak.load(Ordering::SeqCst) > 0 { break; } tokio::time::sleep(Duration::from_millis(20)).await; }
        assert!(t.idle_streak.load(Ordering::SeqCst) >= 1, "healthy again: an idle tick");
        assert!(!t.dormant.load(Ordering::SeqCst));
        std::env::set_var("ANTILOKI_BE_TICK_CHAIN_DELAY_MS", "3600000");
    }
}
