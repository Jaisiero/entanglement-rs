//! ABA race in `server::dispatch_to_workers` (C++).
//!
//! Dispatch used a single bool `m_dispatch_pending`: the dispatcher cleared it
//! when dispatch N finished and set it again when dispatch N+1 started. A
//! worker still waiting to see N's clear (because it was preempted right
//! there) could miss it: it saw the flag set again, kept waiting for N
//! forever, never ran N+1, and the dispatcher waited for its "done" forever.
//! Measured on 2026-10-26: two shards hung after 2 h 40 min and 5 h, tick
//! stopped and ~490 % CPU.
//!
//! The test forces preemption (more workers than cores, plus busy threads)
//! and chains dispatches back to back. A watchdog aborts the process if
//! dispatch stops making progress: a hang is a failure, not an endless test.
use entanglement_rs::EntServer;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static RUNS: AtomicU64 = AtomicU64::new(0);

// On Windows the server does not initialise Winsock (it is only deployed on Linux).
#[cfg(windows)]
#[link(name = "ws2_32")]
extern "system" { fn WSAStartup(version: u16, data: *mut u8) -> i32; }

fn init_network() {
    #[cfg(windows)]
    unsafe {
        let mut data = [0u8; 512];
        assert_eq!(WSAStartup(0x0202, data.as_mut_ptr()), 0, "WSAStartup");
    }
}

extern "C" fn task(_ctx: *mut std::ffi::c_void, _widx: i32) {
    RUNS.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn worker_dispatch_does_not_hang() {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let workers = (cores as i32).clamp(4, 8);
    init_network();
    let server = EntServer::new(47123, "127.0.0.1");
    server.set_worker_count(workers);
    server.start().expect("server starts");
    let n = server.worker_count();
    assert!(n >= 2, "dispatch needs worker threads (there are {n})");

    // Keep the CPU busy so the OS preempts workers often.
    let stop = Arc::new(AtomicBool::new(false));
    let busy: Vec<_> = (0..2).map(|_| {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut x = 0u64;
            while !stop.load(Ordering::Relaxed) { x = x.wrapping_mul(31).wrapping_add(7); }
            x
        })
    }).collect();

    // Watchdog: no progress for 10 s => hang. It stops once dispatching ends.
    let progress = Arc::new(AtomicU64::new(0));
    let watchdog = {
        let progress = progress.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut last = 0;
            let mut since = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                let p = progress.load(Ordering::Relaxed);
                if p != last { last = p; since = Instant::now(); continue; }
                if since.elapsed() > Duration::from_secs(10) {
                    eprintln!("HANG: dispatch_to_workers made no progress after {p} dispatches");
                    std::process::exit(101);
                }
            }
        })
    };

    let mut contexts = vec![std::ptr::null_mut(); n as usize];
    let dispatches: u64 = 20_000;
    for i in 0..dispatches {
        server.dispatch_to_workers(task, &mut contexts);
        progress.store(i + 1, Ordering::Relaxed);
    }
    stop.store(true, Ordering::Relaxed);
    for h in busy { let _ = h.join(); }
    let _ = watchdog.join();
    server.stop();
    assert_eq!(RUNS.load(Ordering::Relaxed), dispatches * n as u64,
        "each dispatch runs the task exactly once per worker");
}
