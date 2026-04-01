use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use entanglement_rs::*;

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
    fn echo_raw(&self, hdr: &EntPacketHeader, payload: &[u8], sender: EntEndpoint) -> bool {
        let mut raw_hdr = hdr.to_raw();
        let ret = unsafe {
            entanglement_sys::ent_server_send_raw_to(
                self.0 as *mut entanglement_sys::ent_server_t,
                &mut raw_hdr,
                payload.as_ptr() as *const std::ffi::c_void,
                entanglement_sys::ent_endpoint { address: sender.address, port: sender.port },
            )
        };
        ret >= 0
    }
}

struct Stats {
    recv: AtomicU64,
    echo: AtomicU64,
    sent: AtomicU64,
    echoes: AtomicU64,
}

fn run_server(port: u16, workers: i32, sockets: i32) {
    println!("=== NET BENCH SERVER ===");
    println!("  Port: {}  Workers: {}  Sockets: {}  AsyncIO: on", port, workers, sockets);
    println!("  Press Ctrl+C to stop\n");

    let stats = Arc::new(Stats {
        recv: AtomicU64::new(0), echo: AtomicU64::new(0),
        sent: AtomicU64::new(0), echoes: AtomicU64::new(0),
    });

    let server = EntServer::new(port, "0.0.0.0");
    server.set_worker_count(workers);
    server.set_use_async_io(true);
    server.set_socket_count(sockets);
    server.register_default_channels();
    server.enable_auto_retransmit();

    let h = SrvHandle::from_server(&server);

    let sr = stats.clone();
    let se = stats.clone();
    server.set_on_client_data(move |hdr, payload, sender| {
        sr.recv.fetch_add(1, Ordering::Relaxed);
        if h.echo(payload.as_ptr(), payload.len(), hdr.channel_id, sender) {
            se.echo.fetch_add(1, Ordering::Relaxed);
        }
    });

    // Bulk coalesced echo: echo the entire coalesced packet raw
    let h_coal = SrvHandle::from_server(&server);
    let sr_coal = stats.clone();
    let se_coal = stats.clone();
    server.set_on_coalesced_data(move |hdr, raw_payload, message_count, sender| {
        sr_coal.recv.fetch_add(message_count as u64, Ordering::Relaxed);
        if h_coal.echo_raw(hdr, raw_payload, sender) {
            se_coal.echo.fetch_add(message_count as u64, Ordering::Relaxed);
        }
    });

    let srv_allocs: AllocMap = Arc::new(Mutex::new(HashMap::new()));
    let am = srv_allocs.clone();
    server.set_on_allocate_message(move |_, _, _, _, max_size| alloc_buf(&am, max_size));

    let h2 = SrvHandle::from_server(&server);
    let sr2 = stats.clone();
    let se2 = stats.clone();
    let cm = srv_allocs.clone();
    server.set_on_message_complete(move |sender, _, ch_id, data, total_size| {
        sr2.recv.fetch_add(1, Ordering::Relaxed);
        if h2.echo(data, total_size, ch_id, sender) {
            se2.echo.fetch_add(1, Ordering::Relaxed);
        }
        free_buf(&cm, data);
    });

    let fm = srv_allocs;
    server.set_on_message_failed(move |_, _, _, buf| free_buf(&fm, buf));

    server.set_on_client_connected(|_, addr, p| {
        println!("[server] +client {}:{}", addr, p);
    });
    server.set_on_client_disconnected(|_, addr, p| {
        println!("[server] -client {}:{}", addr, p);
    });

    server.start().expect("server start failed");
    println!("[server] Listening on 0.0.0.0:{}\n", port);

    let start = Instant::now();
    let mut last_recv = 0u64;
    loop {
        server.poll(8192);
        server.update();

        let elapsed = start.elapsed();
        if elapsed.as_millis() % 1000 < 2 {
            let r = stats.recv.load(Ordering::Relaxed);
            let e = stats.echo.load(Ordering::Relaxed);
            if r != last_recv {
                let dr = r - last_recv;
                println!("  [{:.0}s] recv:{} ({}/s)  echo:{}", elapsed.as_secs_f64(), r, dr, e);
                last_recv = r;
            }
        }
        std::thread::yield_now();
    }
}

fn run_client(server_ip: &str, port: u16, num_clients: usize, duration_secs: u64, payload_size: usize, burst: usize, coalesced: bool) {
    let send_channel: u8 = if coalesced { 4 } else { 1 };
    println!("=== NET BENCH CLIENT ===");
    println!("  Server: {}:{}  Clients: {}  Duration: {}s", server_ip, port, num_clients, duration_secs);
    println!("  Payload: {}B  Burst: {}  Coalesced: {}", payload_size, burst, if coalesced { "YES" } else { "NO" });
    println!("=============================================\n");

    let stats = Arc::new(Stats {
        recv: AtomicU64::new(0), echo: AtomicU64::new(0),
        sent: AtomicU64::new(0), echoes: AtomicU64::new(0),
    });
    let running = Arc::new(AtomicBool::new(true));

    let mut client_threads = Vec::new();

    for i in 0..num_clients {
        let stats_c = stats.clone();
        let running_c = running.clone();
        let ip = server_ip.to_string();
        let psz = payload_size;
        let bst = burst;
        let ch = send_channel;

        client_threads.push(std::thread::spawn(move || {
            let client = EntClient::new(&ip, port);
            client.register_default_channels();

            let connected = Arc::new(AtomicBool::new(false));
            {
                let c = connected.clone();
                client.set_on_connected(move || { c.store(true, Ordering::Relaxed); });
            }
            {
                let sc = stats_c.clone();
                client.set_on_data_received(move |_, _| {
                    sc.echoes.fetch_add(1, Ordering::Relaxed);
                });
            }

            let cli_allocs: AllocMap = Arc::new(Mutex::new(HashMap::new()));
            {
                let am = cli_allocs.clone();
                client.set_on_allocate_message(move |_, _, _, _, max_size| alloc_buf(&am, max_size));
            }
            {
                let sc = stats_c.clone();
                let cm = cli_allocs.clone();
                client.set_on_message_complete(move |_, _, _, data, _| {
                    sc.echoes.fetch_add(1, Ordering::Relaxed);
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
                    eprintln!("[client {}] connection timeout", i);
                    return;
                }
            }
            println!("[client {}] connected", i);

            let buf = vec![0xABu8; psz];
            while running_c.load(Ordering::Relaxed) {
                client.poll(8192);
                client.update();
                if client.can_send() {
                    for _ in 0..bst {
                        if !client.can_send() { break; }
                        if client.send(&buf, ch, 0).is_ok() {
                            stats_c.sent.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                std::thread::yield_now();
            }
            client.disconnect();
        }));
    }

    let start = Instant::now();
    let mut last_sent = 0u64;
    let mut last_echo = 0u64;

    while start.elapsed() < Duration::from_secs(duration_secs) {
        std::thread::sleep(Duration::from_secs(1));
        let s = stats.sent.load(Ordering::Relaxed);
        let e = stats.echoes.load(Ordering::Relaxed);
        let ds = s - last_sent;
        let de = e - last_echo;
        println!("  [{:.0}s] send:{}/s  echo:{}/s  total_sent:{}  total_echo:{}",
            start.elapsed().as_secs_f64(), ds, de, s, e);
        last_sent = s;
        last_echo = e;
    }

    running.store(false, Ordering::Relaxed);
    for t in client_threads { t.join().ok(); }

    let elapsed = start.elapsed().as_secs_f64();
    let total_sent = stats.sent.load(Ordering::Relaxed);
    let total_echo = stats.echoes.load(Ordering::Relaxed);
    let bw_send = total_sent as f64 * payload_size as f64 * 8.0 / elapsed / 1_000_000.0;

    println!("\n=============================================");
    println!("  CLIENT RESULTS ({} clients, {}B payload)", num_clients, payload_size);
    println!("  Duration:    {:.1}s", elapsed);
    println!("  Sent:        {} ({:.0}/s)", total_sent, total_sent as f64 / elapsed);
    println!("  Echoes:      {} ({:.0}/s)", total_echo, total_echo as f64 / elapsed);
    println!("  Echo rate:   {:.1}%", if total_sent > 0 { 100.0 * total_echo as f64 / total_sent as f64 } else { 0.0 });
    println!("  BW send:     {:.0} Mbps", bw_send);
    println!("=============================================");
}

fn main() {
    // Windows requires Winsock initialization before any socket calls
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "ws2_32")]
        unsafe extern "system" {
            fn WSAStartup(wVersionRequired: u16, lpWSAData: *mut u8) -> i32;
        }
        let mut wsa_data = [0u8; 512]; // WSADATA is ~408 bytes on x64
        let ret = WSAStartup(0x0202, wsa_data.as_mut_ptr());
        if ret != 0 { panic!("WSAStartup failed: {}", ret); }
    }

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  net_bench server [port] [workers] [sockets]");
        eprintln!("  net_bench client <server_ip> [port] [clients] [duration] [payload] [burst] [coalesced]");
        eprintln!("  coalesced: 0 or 1 (default 0) — use coalesced channel");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "server" => {
            let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(19877);
            let workers: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
            let sockets: i32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4);
            run_server(port, workers, sockets);
        }
        "client" => {
            let ip = args.get(2).map(|s| s.as_str()).unwrap_or("127.0.0.1");
            let port: u16 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(19877);
            let clients: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4);
            let duration: u64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(10);
            let payload: usize = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(1100);
            let burst: usize = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(32);
            let coalesced: bool = args.get(8).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) != 0;
            run_client(ip, port, clients, duration, payload, burst, coalesced);
        }
        _ => {
            eprintln!("Unknown mode: {}. Use 'server' or 'client'", args[1]);
            std::process::exit(1);
        }
    }
}
