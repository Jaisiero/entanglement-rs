use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicU32, AtomicBool, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use entanglement_rs::*;

const CH_UNRELIABLE: u8 = 1;
const CH_RELIABLE: u8 = 2;
const CH_ORDERED: u8 = 3;

const S_UNR: usize = 0;
const S_REL: usize = 1;
const S_ORD: usize = 2;
const F_UNR: usize = 3;
const F_REL: usize = 4;
const F_ORD: usize = 5;
const NUM_STREAMS: usize = 6;

const SIMPLE_SIZE: usize = 9;
const FRAG_SIZE: usize = 2500;

const STREAM_NAMES: [&str; NUM_STREAMS] = [
    "simple-unreliable", "simple-reliable", "simple-ordered",
    "frag-unreliable",   "frag-reliable",   "frag-ordered",
];
const STREAM_CHANNELS: [u8; NUM_STREAMS] = [
    CH_UNRELIABLE, CH_RELIABLE, CH_ORDERED,
    CH_UNRELIABLE, CH_RELIABLE, CH_ORDERED,
];
const STREAM_IS_FRAG: [bool; NUM_STREAMS] = [false, false, false, true, true, true];

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SoakMsg {
    msg_id: u32,
    total_expected: u32,
    stream_idx: u8,
}

struct StreamStats {
    sent: AtomicU64,
    echoes: AtomicU64,
    last_echo_id: AtomicU32,
    violations: AtomicU64,
}

struct Stats {
    streams: [StreamStats; NUM_STREAMS],
    losses: AtomicU64,
    server_recv: AtomicU64,
    server_echo: AtomicU64,
}

type AllocMap = Arc<Mutex<HashMap<usize, usize>>>;

fn alloc_buf(map: &AllocMap, max_size: usize) -> *mut u8 {
    let mut buf = vec![0u8; max_size];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    map.lock().unwrap().insert(ptr as usize, max_size);
    ptr
}

fn free_buf(map: &AllocMap, ptr: *mut u8) {
    if let Some(cap) = map.lock().unwrap().remove(&(ptr as usize)) {
        unsafe { drop(Vec::from_raw_parts(ptr, cap, cap)); }
    }
}

fn fmt_pct(n: u64, d: u64) -> String {
    if d == 0 { "N/A".into() } else { format!("{:.1}%", 100.0 * n as f64 / d as f64) }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let duration_secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(19876);

    println!("=============================================");
    println!("  Entanglement Rust Soak Test (low-level)");
    println!("  Duration: {}s   Port: {}", duration_secs, port);
    println!("  Streams: {} (3 channels x 2 sizes)", NUM_STREAMS);
    println!("  Simple: {} B   Frag: {} B", SIMPLE_SIZE, FRAG_SIZE);
    println!("=============================================\n");

    let stats = Arc::new(Stats {
        streams: std::array::from_fn(|_| StreamStats {
            sent: AtomicU64::new(0), echoes: AtomicU64::new(0),
            last_echo_id: AtomicU32::new(0), violations: AtomicU64::new(0),
        }),
        losses: AtomicU64::new(0),
        server_recv: AtomicU64::new(0),
        server_echo: AtomicU64::new(0),
    });
    let running = Arc::new(AtomicBool::new(true));

    // ── Echo server thread ──
    let (echo_tx, echo_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, u8, EntEndpoint)>(65536);
    let echo_tx = Arc::new(Mutex::new(echo_tx));

    let stats_s = stats.clone();
    let running_s = running.clone();
    let server_thread = std::thread::spawn(move || {
        let server = EntServer::new(port, "0.0.0.0");
        server.set_worker_count(4);
        server.register_default_channels();
        server.enable_auto_retransmit();

        // Simple message data callback
        let tx = echo_tx.clone();
        let sr = stats_s.clone();
        server.set_on_client_data(move |hdr, payload, sender| {
            sr.server_recv.fetch_add(1, Ordering::Relaxed);
            if let Ok(g) = tx.lock() {
                let _ = g.send((payload.to_vec(), hdr.channel_id, sender));
            }
        });

        // Fragment reassembly callbacks
        let srv_allocs: AllocMap = Arc::new(Mutex::new(HashMap::new()));

        let am = srv_allocs.clone();
        server.set_on_allocate_message(move |_sender, _msg_id, _ch_id, _frag_count, max_size| {
            alloc_buf(&am, max_size)
        });

        let tx2 = echo_tx.clone();
        let sr2 = stats_s.clone();
        let cm = srv_allocs.clone();
        server.set_on_message_complete(move |sender, _msg_id, ch_id, data, total_size| {
            sr2.server_recv.fetch_add(1, Ordering::Relaxed);
            let slice = unsafe { std::slice::from_raw_parts(data, total_size) };
            if let Ok(g) = tx2.lock() {
                let _ = g.send((slice.to_vec(), ch_id, sender));
            }
            free_buf(&cm, data);
        });

        let fm = srv_allocs.clone();
        server.set_on_message_failed(move |_sender, _msg_id, _ch_id, app_buffer| {
            free_buf(&fm, app_buffer);
        });

        server.set_on_client_connected(|_ep, addr, p| {
            println!("[server] Client connected: {}:{}", addr, p);
        });
        server.set_on_client_disconnected(|_ep, addr, p| {
            println!("[server] Client disconnected: {}:{}", addr, p);
        });

        server.start().expect("server start failed");
        println!("[server] Listening on 0.0.0.0:{} (4 workers)", port);

        while running_s.load(Ordering::Relaxed) {
            while let Ok((data, ch, dest)) = echo_rx.try_recv() {
                // send_to auto-fragments if data > MTU
                if server.send_to(&data, ch, dest, 0).is_ok() {
                    stats_s.server_echo.fetch_add(1, Ordering::Relaxed);
                }
            }
            server.poll(4096);
            server.update();
            std::thread::sleep(Duration::from_micros(500));
        }
        server.stop();
        println!("[server] Stopped");
    });

    std::thread::sleep(Duration::from_millis(500));

    // ── Client ──
    let client = EntClient::new("127.0.0.1", port);
    client.register_default_channels();
    client.enable_auto_retransmit();

    let connected = Arc::new(AtomicBool::new(false));
    {
        let c = connected.clone();
        client.set_on_connected(move || { c.store(true, Ordering::Relaxed); });
    }

    // Simple message echo callback
    {
        let st = stats.clone();
        client.set_on_data_received(move |_hdr, payload| {
            if payload.len() >= SIMPLE_SIZE {
                let msg: SoakMsg = unsafe { std::ptr::read_unaligned(payload.as_ptr() as *const SoakMsg) };
                let idx = msg.stream_idx as usize;
                if idx < NUM_STREAMS {
                    st.streams[idx].echoes.fetch_add(1, Ordering::Relaxed);
                    if STREAM_CHANNELS[idx] == CH_ORDERED {
                        let prev = st.streams[idx].last_echo_id.swap(msg.msg_id, Ordering::Relaxed);
                        if msg.msg_id <= prev && prev > 0 {
                            st.streams[idx].violations.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        });
    }

    // Fragment reassembly callbacks for client (receiving echoed fragments)
    let cli_allocs: AllocMap = Arc::new(Mutex::new(HashMap::new()));

    {
        let am = cli_allocs.clone();
        client.set_on_allocate_message(move |_sender, _msg_id, _ch_id, _frag_count, max_size| {
            alloc_buf(&am, max_size)
        });
    }
    {
        let st = stats.clone();
        let cm = cli_allocs.clone();
        client.set_on_message_complete(move |_sender, _msg_id, _ch_id, data, total_size| {
            let slice = unsafe { std::slice::from_raw_parts(data, total_size) };
            if slice.len() >= SIMPLE_SIZE {
                let msg: SoakMsg = unsafe { std::ptr::read_unaligned(slice.as_ptr() as *const SoakMsg) };
                let idx = msg.stream_idx as usize;
                if idx < NUM_STREAMS {
                    st.streams[idx].echoes.fetch_add(1, Ordering::Relaxed);
                    if STREAM_CHANNELS[idx] == CH_ORDERED {
                        let prev = st.streams[idx].last_echo_id.swap(msg.msg_id, Ordering::Relaxed);
                        if msg.msg_id <= prev && prev > 0 {
                            st.streams[idx].violations.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
            free_buf(&cm, data);
        });
    }
    {
        let fm = cli_allocs.clone();
        client.set_on_message_failed(move |_sender, _msg_id, _ch_id, app_buffer| {
            free_buf(&fm, app_buffer);
        });
    }

    {
        let st = stats.clone();
        client.set_on_packet_lost(move |_info| { st.losses.fetch_add(1, Ordering::Relaxed); });
    }

    client.connect().expect("connect failed");
    println!("[client] Connecting to 127.0.0.1:{}...", port);

    let t0 = Instant::now();
    while !connected.load(Ordering::Relaxed) {
        client.poll(256);
        client.update();
        std::thread::sleep(Duration::from_millis(10));
        if t0.elapsed() > Duration::from_secs(10) {
            eprintln!("FATAL: connection timeout");
            running.store(false, Ordering::Relaxed);
            server_thread.join().ok();
            return;
        }
    }
    println!("[client] Connected\n");
    println!("=== Sending for {} seconds ===\n", duration_secs);

    // ── Send loop ──
    let start = Instant::now();
    let mut counters = [0u32; NUM_STREAMS];
    let mut total_sent: u64 = 0;
    let mut last_report = Instant::now();

    while start.elapsed() < Duration::from_secs(duration_secs) {
        client.poll(4096);
        client.update();

        if !client.can_send() {
            std::thread::sleep(Duration::from_micros(100));
            continue;
        }

        for idx in 0..NUM_STREAMS {
            if !client.can_send() { break; }
            if STREAM_IS_FRAG[idx] && client.is_fragment_throttled() {
                continue;
            }

            counters[idx] += 1;
            let msg = SoakMsg { msg_id: counters[idx], total_expected: 0, stream_idx: idx as u8 };
            let sz = if STREAM_IS_FRAG[idx] { FRAG_SIZE } else { SIMPLE_SIZE };
            let mut buf = vec![0u8; sz];
            unsafe { std::ptr::write_unaligned(buf.as_mut_ptr() as *mut SoakMsg, msg); }
            for i in SIMPLE_SIZE..sz { buf[i] = (i & 0xFF) as u8; }

            // Regular send() -- library auto-fragments if > MTU
            if client.send(&buf, STREAM_CHANNELS[idx], 0).is_ok() {
                stats.streams[idx].sent.fetch_add(1, Ordering::Relaxed);
                total_sent += 1;
            }
        }

        if last_report.elapsed() >= Duration::from_secs(2) {
            let el = start.elapsed().as_secs_f64();
            let ech: u64 = stats.streams.iter().map(|s| s.echoes.load(Ordering::Relaxed)).sum();
            let cg = client.congestion();
            println!("  [{:.0}s] sent:{} echoes:{} loss:{} cwnd:{} inflight:{} {:.0} msg/s",
                el, total_sent, ech, stats.losses.load(Ordering::Relaxed),
                cg.cwnd, cg.in_flight, total_sent as f64 / el);
            last_report = Instant::now();
        }
    }

    let send_elapsed = start.elapsed();
    println!("\n=== Send complete ({:.1}s) -- {} messages ===", send_elapsed.as_secs_f64(), total_sent);

    // ── Drain ──
    println!("  Draining echoes...");
    let drain_t = Instant::now();
    let mut prev: u64 = 0;
    loop {
        for _ in 0..200 {
            client.poll(4096);
            client.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        let cur: u64 = stats.streams.iter().map(|s| s.echoes.load(Ordering::Relaxed)).sum();
        if cur == prev || drain_t.elapsed() > Duration::from_secs(15) { break; }
        println!("  [drain {:.0}s] echoes: {}", drain_t.elapsed().as_secs_f64(), cur);
        prev = cur;
    }

    let cg = client.congestion();
    client.disconnect();
    std::thread::sleep(Duration::from_millis(500));
    running.store(false, Ordering::Relaxed);
    server_thread.join().ok();

    // ── Results ──
    let total_elapsed = start.elapsed();
    let total_echoes: u64 = stats.streams.iter().map(|s| s.echoes.load(Ordering::Relaxed)).sum();

    println!("\n=============================================");
    println!("  SOAK TEST RESULTS");
    println!("  Send duration:   {:.1}s", send_elapsed.as_secs_f64());
    println!("  Total duration:  {:.1}s", total_elapsed.as_secs_f64());
    println!("  Total sent:      {}", total_sent);
    println!("  Total echoes:    {}", total_echoes);
    println!("  Throughput:      {:.0} msg/s (send)  {:.0} msg/s (echo)",
        total_sent as f64 / send_elapsed.as_secs_f64(),
        total_echoes as f64 / total_elapsed.as_secs_f64());
    println!("  Losses detected: {}", stats.losses.load(Ordering::Relaxed));
    println!("  Server recv:     {}", stats.server_recv.load(Ordering::Relaxed));
    println!("  Server echoed:   {}", stats.server_echo.load(Ordering::Relaxed));
    println!("  Congestion:      cwnd={} inflight={} ssthresh={}", cg.cwnd, cg.in_flight, cg.ssthresh);
    println!("=============================================\n");

    println!("  --- Simple Streams ---");
    for idx in [S_UNR, S_REL, S_ORD] {
        let s = stats.streams[idx].sent.load(Ordering::Relaxed);
        let e = stats.streams[idx].echoes.load(Ordering::Relaxed);
        print!("  {:24} {} / {} ({})", STREAM_NAMES[idx], e, s, fmt_pct(e, s));
        if idx == S_ORD { print!("  viol:{}", stats.streams[idx].violations.load(Ordering::Relaxed)); }
        println!();
    }
    println!("\n  --- Fragmented Streams ---");
    for idx in [F_UNR, F_REL, F_ORD] {
        let s = stats.streams[idx].sent.load(Ordering::Relaxed);
        let e = stats.streams[idx].echoes.load(Ordering::Relaxed);
        print!("  {:24} {} / {} ({})", STREAM_NAMES[idx], e, s, fmt_pct(e, s));
        if idx == F_ORD { print!("  viol:{}", stats.streams[idx].violations.load(Ordering::Relaxed)); }
        println!();
    }

    // ── Verdicts ──
    println!("\n=============================================");
    let mut all_pass = true;
    for (label, idx) in [
        ("SIMPLE RELIABLE DELIVERED", S_REL), ("SIMPLE ORDERED DELIVERED", S_ORD),
        ("FRAG RELIABLE DELIVERED",   F_REL), ("FRAG ORDERED DELIVERED",   F_ORD),
    ] {
        let s = stats.streams[idx].sent.load(Ordering::Relaxed);
        let e = stats.streams[idx].echoes.load(Ordering::Relaxed);
        if s == 0      { println!("  {:36} [SKIP]", label); }
        else if e == s { println!("  {:36} [PASS]", label); }
        else           { println!("  {:36} [FAIL] ({} missing)", label, s - e); all_pass = false; }
    }
    for (label, idx) in [("SIMPLE ORDERED ORDER", S_ORD), ("FRAG ORDERED ORDER", F_ORD)] {
        let v = stats.streams[idx].violations.load(Ordering::Relaxed);
        if v == 0 { println!("  {:36} [PASS]", label); }
        else      { println!("  {:36} [WARN] ({} reorder)", label, v); }
    }
    for (label, idx) in [("SIMPLE UNRELIABLE DELIVERY", S_UNR), ("FRAG UNRELIABLE DELIVERY", F_UNR)] {
        let s = stats.streams[idx].sent.load(Ordering::Relaxed);
        let e = stats.streams[idx].echoes.load(Ordering::Relaxed);
        println!("  {:36} {}", label, fmt_pct(e, s));
    }
    println!("=============================================");
    if all_pass { println!("  OVERALL: [PASS]"); }
    else        { println!("  OVERALL: [FAIL]"); std::process::exit(1); }
    println!("=============================================");
}