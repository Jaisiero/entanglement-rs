use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use entanglement_rs::*;

const CH_UNRELIABLE: u8 = 1;

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

struct PerfStats {
    client_sent: AtomicU64,
    client_echoes: AtomicU64,
    server_recv: AtomicU64,
    server_echo: AtomicU64,
}

// Wrap raw pointer so it can be captured by Send+Sync closures
#[derive(Clone, Copy)]
struct SrvHandle(usize);
unsafe impl Send for SrvHandle {}
unsafe impl Sync for SrvHandle {}

impl SrvHandle {
    fn from_server(server: &EntServer) -> Self {
        SrvHandle(server.raw_handle() as usize)
    }
    fn echo(&self, payload: *const u8, len: usize, ch: u8, sender: EntEndpoint) -> bool {
        let mut msg_id: u32 = 0;
        let ret = unsafe {
            entanglement_sys::ent_server_send_to(
                self.0 as *mut entanglement_sys::ent_server_t,
                payload as *const std::ffi::c_void,
                len, ch,
                entanglement_sys::ent_endpoint { address: sender.address, port: sender.port },
                0, &mut msg_id,
            )
        };
        ret >= 0
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let duration_secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let num_clients: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let payload_size: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1100);
    let burst: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(32);
    let port: u16 = 19877;

    println!("=============================================");
    println!("  Entanglement Rust Perf Benchmark");
    println!("  Duration: {}s  Clients: {}  Payload: {}B", duration_secs, num_clients, payload_size);
    println!("  Burst: {}  Port: {}  Channel: UNRELIABLE", burst, port);
    println!("=============================================\n");

    let stats = Arc::new(PerfStats {
        client_sent: AtomicU64::new(0),
        client_echoes: AtomicU64::new(0),
        server_recv: AtomicU64::new(0),
        server_echo: AtomicU64::new(0),
    });
    let running = Arc::new(AtomicBool::new(true));

    // ── Server thread with DIRECT echo ──
    let stats_s = stats.clone();
    let running_s = running.clone();
    let server_thread = std::thread::spawn(move || {
        let server = EntServer::new(port, "0.0.0.0");
        server.set_worker_count(4);
        server.register_default_channels();

        let h = SrvHandle::from_server(&server);

        // Direct echo for simple messages -- no channel, no mutex
        let sr = stats_s.clone();
        let se = stats_s.clone();
        server.set_on_client_data(move |hdr, payload, sender| {
            sr.server_recv.fetch_add(1, Ordering::Relaxed);
            if h.echo(payload.as_ptr(), payload.len(), hdr.channel_id, sender) {
                se.server_echo.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Fragment support
        let srv_allocs: AllocMap = Arc::new(Mutex::new(HashMap::new()));
        let am = srv_allocs.clone();
        server.set_on_allocate_message(move |_, _, _, _, max_size| alloc_buf(&am, max_size));

        let h2 = SrvHandle::from_server(&server);
        let sr2 = stats_s.clone();
        let se2 = stats_s.clone();
        let cm = srv_allocs.clone();
        server.set_on_message_complete(move |sender, _, ch_id, data, total_size| {
            sr2.server_recv.fetch_add(1, Ordering::Relaxed);
            if h2.echo(data, total_size, ch_id, sender) {
                se2.server_echo.fetch_add(1, Ordering::Relaxed);
            }
            free_buf(&cm, data);
        });

        let fm = srv_allocs;
        server.set_on_message_failed(move |_, _, _, buf| free_buf(&fm, buf));

        server.set_on_client_connected(|_, addr, p| {
            println!("[server] +client {}:{}", addr, p);
        });

        server.start().expect("server start failed");
        println!("[server] Listening on 0.0.0.0:{} (4 workers)\n", port);

        while running_s.load(Ordering::Relaxed) {
            server.poll(8192);
            server.update();
            std::thread::yield_now();
        }
        server.stop();
    });

    std::thread::sleep(Duration::from_millis(300));

    // ── Client threads ──
    let mut client_threads = Vec::new();

    for i in 0..num_clients {
        let stats_c = stats.clone();
        let running_c = running.clone();
        let psz = payload_size;
        let bst = burst;

        client_threads.push(std::thread::spawn(move || {
            let client = EntClient::new("127.0.0.1", port);
            client.register_default_channels();

            let connected = Arc::new(AtomicBool::new(false));
            {
                let c = connected.clone();
                client.set_on_connected(move || { c.store(true, Ordering::Relaxed); });
            }
            {
                let sc = stats_c.clone();
                client.set_on_data_received(move |_, _| {
                    sc.client_echoes.fetch_add(1, Ordering::Relaxed);
                });
            }

            // Fragment support
            let cli_allocs: AllocMap = Arc::new(Mutex::new(HashMap::new()));
            {
                let am = cli_allocs.clone();
                client.set_on_allocate_message(move |_, _, _, _, max_size| alloc_buf(&am, max_size));
            }
            {
                let sc = stats_c.clone();
                let cm = cli_allocs.clone();
                client.set_on_message_complete(move |_, _, _, data, _| {
                    sc.client_echoes.fetch_add(1, Ordering::Relaxed);
                    free_buf(&cm, data);
                });
            }
            {
                let fm = cli_allocs;
                client.set_on_message_failed(move |_, _, _, buf| free_buf(&fm, buf));
            }

            client.connect().expect("connect failed");
            let t0 = Instant::now();
            while !connected.load(Ordering::Relaxed) {
                client.poll(256);
                client.update();
                std::thread::sleep(Duration::from_millis(5));
                if t0.elapsed() > Duration::from_secs(5) {
                    eprintln!("[client {}] timeout", i);
                    return;
                }
            }

            let buf = vec![0xABu8; psz];
            while running_c.load(Ordering::Relaxed) {
                client.poll(8192);
                client.update();
                if client.can_send() {
                    for _ in 0..bst {
                        if !client.can_send() { break; }
                        if client.send(&buf, CH_UNRELIABLE, 0).is_ok() {
                            stats_c.client_sent.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                std::thread::yield_now();
            }
            client.disconnect();
        }));
    }

    // ── Reporter ──
    let start = Instant::now();
    let mut last_sent = 0u64;
    let mut last_echo = 0u64;

    while start.elapsed() < Duration::from_secs(duration_secs) {
        std::thread::sleep(Duration::from_secs(1));
        let s = stats.client_sent.load(Ordering::Relaxed);
        let e = stats.client_echoes.load(Ordering::Relaxed);
        let sr = stats.server_recv.load(Ordering::Relaxed);
        let se = stats.server_echo.load(Ordering::Relaxed);
        let ds = s - last_sent;
        let de = e - last_echo;
        let el = start.elapsed().as_secs_f64();
        println!("  [{:.0}s] send:{}/s  echo:{}/s  total:{}  srv_recv:{}  srv_echo:{}",
            el, ds, de, s, sr, se);
        last_sent = s;
        last_echo = e;
    }

    running.store(false, Ordering::Relaxed);
    for t in client_threads { t.join().ok(); }
    std::thread::sleep(Duration::from_millis(300));
    server_thread.join().ok();

    let elapsed = start.elapsed().as_secs_f64();
    let total_sent = stats.client_sent.load(Ordering::Relaxed);
    let total_echo = stats.client_echoes.load(Ordering::Relaxed);
    let total_srv = stats.server_recv.load(Ordering::Relaxed);
    let total_se = stats.server_echo.load(Ordering::Relaxed);
    let bw_send = total_sent as f64 * payload_size as f64 * 8.0 / elapsed / 1_000_000.0;
    let bw_echo = total_echo as f64 * payload_size as f64 * 8.0 / elapsed / 1_000_000.0;

    println!("\n=============================================");
    println!("  PERF RESULTS ({} clients, {}B payload)", num_clients, payload_size);
    println!("  Duration:      {:.1}s", elapsed);
    println!("  Client sent:   {} ({:.0}/s)", total_sent, total_sent as f64 / elapsed);
    println!("  Client echoes: {} ({:.0}/s)", total_echo, total_echo as f64 / elapsed);
    println!("  Server recv:   {} ({:.0}/s)", total_srv, total_srv as f64 / elapsed);
    println!("  Server echo:   {} ({:.0}/s)", total_se, total_se as f64 / elapsed);
    println!("  Echo rate:     {:.1}%", if total_sent > 0 { 100.0 * total_echo as f64 / total_sent as f64 } else { 0.0 });
    println!("  BW send:       {:.0} Mbps", bw_send);
    println!("  BW echo:       {:.0} Mbps", bw_echo);
    println!("=============================================");
}