// ============================================================================
// Entanglement Rust — Stress Test  (N virtual clients in a single thread)
// ============================================================================
//
// Mirrors the C++ EntanglementStress test for apples-to-apples comparison.
//
// Usage: stress_test [-s ip] [-p port] [-c clients] [-t seconds]
//                    [-r rate_hz] [-v]
//
// All N clients live in one thread with a single polling loop.
// Each client opens a RELIABLE channel and sends at rate_hz per second.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use entanglement_rs::*;

// RELIABLE channel mode
const ENT_CHANNEL_RELIABLE: u32 = 1;

struct VirtualClient {
    client: EntClient,
    connected: bool,
    sent: u32,
    echoes: Box<AtomicU32>,
    losses: Box<AtomicU32>,
    channel_id: i32,
}

#[repr(C, packed)]
#[derive(Copy, Clone)]
struct StressMsg {
    client_index: u32,
    sequence: u32,
}

fn main() {
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "ws2_32")]
        unsafe extern "system" {
            fn WSAStartup(wVersionRequired: u16, lpWSAData: *mut u8) -> i32;
        }
        let mut wsa_data = [0u8; 512];
        let ret = WSAStartup(0x0202, wsa_data.as_mut_ptr());
        if ret != 0 { panic!("WSAStartup failed: {}", ret); }
    }

    let args: Vec<String> = std::env::args().collect();

    let mut server_addr = "127.0.0.1".to_string();
    let mut port: u16 = 9876;
    let mut num_clients: usize = 100;
    let mut duration_s: u64 = 30;
    let mut rate_hz: u64 = 10;
    let mut verbose = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-s" if i + 1 < args.len() => { server_addr = args[i + 1].clone(); i += 1; }
            "-p" if i + 1 < args.len() => { port = args[i + 1].parse().unwrap_or(9876); i += 1; }
            "-c" if i + 1 < args.len() => { num_clients = args[i + 1].parse().unwrap_or(100); i += 1; }
            "-t" if i + 1 < args.len() => { duration_s = args[i + 1].parse().unwrap_or(30); i += 1; }
            "-r" if i + 1 < args.len() => { rate_hz = args[i + 1].parse().unwrap_or(10); i += 1; }
            "-v" => { verbose = true; }
            _ => {}
        }
        i += 1;
    }

    let target_rate = num_clients as u64 * rate_hz;

    println!("=============================================");
    println!(" Entanglement Stress Test (Rust)");
    println!(" Server:   {}:{}", server_addr, port);
    println!(" Clients:  {}", num_clients);
    println!(" Duration: {}s", duration_s);
    println!(" Rate:     {} Hz per client", rate_hz);
    println!(" Target:   {} pkt/s total", target_rate);
    println!("=============================================");

    // --- Phase 1: Connect all clients (batched) ---
    let mut clients: Vec<VirtualClient> = Vec::with_capacity(num_clients);
    let mut connect_ok = 0usize;
    let mut connect_fail = 0usize;

    let connect_batch = 50;
    let connect_start = Instant::now();

    let mut base = 0;
    while base < num_clients {
        let batch_end = (base + connect_batch).min(num_clients);

        for _idx in base..batch_end {
            let client = EntClient::new(&server_addr, port);
            client.set_verbose(false);
            client.register_default_channels();
            client.enable_auto_retransmit();

            if client.connect().is_err() {
                connect_fail += 1;
                // Still need a placeholder
                clients.push(VirtualClient {
                    client,
                    connected: false,
                    sent: 0,
                    echoes: Box::new(AtomicU32::new(0)),
                    losses: Box::new(AtomicU32::new(0)),
                    channel_id: -1,
                });
                continue;
            }

            // Wait for handshake (brief polling)
            let t0 = Instant::now();
            let mut did_connect = false;
            while t0.elapsed() < Duration::from_secs(5) {
                client.poll(256);
                client.update();
                if client.is_connected() {
                    did_connect = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));

                // Keep earlier clients alive
                for prev in clients.iter() {
                    if prev.connected {
                        prev.client.poll(64);
                        prev.client.update();
                    }
                }
            }

            if !did_connect {
                connect_fail += 1;
                clients.push(VirtualClient {
                    client,
                    connected: false,
                    sent: 0,
                    echoes: Box::new(AtomicU32::new(0)),
                    losses: Box::new(AtomicU32::new(0)),
                    channel_id: -1,
                });
                continue;
            }

            // Open reliable channel
            let ch_id = client.open_channel(ENT_CHANNEL_RELIABLE, 128, "stress");
            if ch_id < 0 {
                connect_fail += 1;
                clients.push(VirtualClient {
                    client,
                    connected: false,
                    sent: 0,
                    echoes: Box::new(AtomicU32::new(0)),
                    losses: Box::new(AtomicU32::new(0)),
                    channel_id: -1,
                });
                continue;
            }

            connect_ok += 1;

            let echoes = Box::new(AtomicU32::new(0));
            let losses = Box::new(AtomicU32::new(0));

            // Register callbacks — use raw pointers to atomics
            let echo_ptr = &*echoes as *const AtomicU32 as usize;
            client.set_on_data_received(move |_, _| {
                unsafe { &*(echo_ptr as *const AtomicU32) }.fetch_add(1, Ordering::Relaxed);
            });

            let loss_ptr = &*losses as *const AtomicU32 as usize;
            client.set_on_packet_lost(move |_| {
                unsafe { &*(loss_ptr as *const AtomicU32) }.fetch_add(1, Ordering::Relaxed);
            });

            clients.push(VirtualClient {
                client,
                connected: true,
                sent: 0,
                echoes,
                losses,
                channel_id: ch_id,
            });
        }

        if verbose || (base + connect_batch) % 100 == 0 || batch_end == num_clients {
            println!("  Connected: {} / {} (fail={})", connect_ok, batch_end, connect_fail);
        }

        // Keep earlier clients alive
        for j in 0..base {
            if clients[j].connected && clients[j].client.is_connected() {
                clients[j].client.poll(64);
                clients[j].client.update();
            }
        }

        std::thread::sleep(Duration::from_millis(10));
        base += connect_batch;
    }

    let connect_elapsed = connect_start.elapsed().as_millis();
    println!();
    println!(" Connected: {} / {} (fail={}) in {} ms", connect_ok, num_clients, connect_fail, connect_elapsed);

    if connect_ok == 0 {
        eprintln!(" No clients connected — aborting.");
        return;
    }

    // --- Phase 2: Steady-state send/recv loop ---
    println!(" Running for {}s...", duration_s);

    let test_start = Instant::now();
    let test_duration = Duration::from_secs(duration_s);
    let send_interval = Duration::from_micros(1_000_000 / rate_hz.max(1));

    // Stagger initial sends
    let mut last_send: Vec<Instant> = (0..num_clients)
        .map(|i| {
            let stagger_us = (send_interval.as_micros() as u64 * i as u64) / num_clients.max(1) as u64;
            test_start + Duration::from_micros(stagger_us)
        })
        .collect();

    let report_interval = Duration::from_secs(5);
    let mut next_report = test_start + report_interval;

    let mut total_sent: u64 = 0;
    let mut poll_cycles: u64 = 0;

    while test_start.elapsed() < test_duration {
        let now = Instant::now();
        let mut sends_this_cycle = 0u64;

        for i in 0..num_clients {
            let vc = &mut clients[i];
            if !vc.connected || !vc.client.is_connected() {
                continue;
            }

            // Poll for incoming
            vc.client.poll(64);
            vc.client.update();

            // Send if it's time
            if now >= last_send[i] {
                if vc.client.can_send() {
                    let msg = StressMsg {
                        client_index: i as u32,
                        sequence: vc.sent,
                    };
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            &msg as *const StressMsg as *const u8,
                            std::mem::size_of::<StressMsg>(),
                        )
                    };
                    if vc.client.send(bytes, vc.channel_id as u8, 0).is_ok() {
                        vc.sent += 1;
                        sends_this_cycle += 1;
                    }
                }
                last_send[i] = now + send_interval;
            }
        }

        total_sent += sends_this_cycle;
        poll_cycles += 1;

        // Periodic report
        if now >= next_report {
            let elapsed_s = test_start.elapsed().as_secs().max(1);
            let mut echo_count: u64 = 0;
            let mut loss_count: u64 = 0;
            for vc in clients.iter() {
                if !vc.connected { continue; }
                echo_count += vc.echoes.load(Ordering::Relaxed) as u64;
                loss_count += vc.losses.load(Ordering::Relaxed) as u64;
            }
            println!("  [{}s] sent={} echoes={} rate={} pkt/s losses={} cycles={}",
                elapsed_s, total_sent, echo_count, total_sent / elapsed_s,
                loss_count, poll_cycles);
            next_report = now + report_interval;
        }

        if sends_this_cycle == 0 {
            std::thread::yield_now();
        }
    }

    // --- Drain phase ---
    println!(" Draining...");
    let drain_end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < drain_end {
        for vc in clients.iter() {
            if !vc.connected || !vc.client.is_connected() { continue; }
            vc.client.poll(64);
            vc.client.update();
        }
        std::thread::yield_now();
    }

    // --- Collect final stats ---
    let mut final_sent: u64 = 0;
    let mut final_echoes: u64 = 0;
    let mut final_losses: u64 = 0;
    let mut still_connected = 0usize;

    for vc in clients.iter() {
        if !vc.connected { continue; }
        final_sent += vc.sent as u64;
        final_echoes += vc.echoes.load(Ordering::Relaxed) as u64;
        final_losses += vc.losses.load(Ordering::Relaxed) as u64;
        if vc.client.is_connected() {
            still_connected += 1;
        }
    }

    // --- Disconnect ---
    println!(" Disconnecting...");
    for vc in clients.iter() {
        if vc.connected {
            vc.client.disconnect();
        }
    }

    // --- Report ---
    let send_rate = final_sent as f64 / duration_s.max(1) as f64;
    let delivery = if final_sent > 0 {
        100.0 * (final_sent - final_losses) as f64 / final_sent as f64
    } else {
        0.0
    };

    println!();
    println!("=============================================");
    println!(" STRESS TEST RESULTS (Rust)");
    println!("=============================================");
    println!(" Clients attempted:  {}", num_clients);
    println!(" Connected:          {}", connect_ok);
    println!(" Still connected:    {}", still_connected);
    println!(" Connection failures:{}", connect_fail);
    println!(" Send duration:      {}s", duration_s);
    println!(" Packets sent:       {}", final_sent);
    println!(" Echoes received:    {}", final_echoes);
    println!(" Losses detected:    {}", final_losses);
    println!(" Delivery:           {:.1}%", delivery);
    println!(" Avg send rate:      {:.0} pkt/s", send_rate);
    println!(" Target rate:        {} pkt/s", connect_ok as u64 * rate_hz);
    println!(" Poll cycles:        {}", poll_cycles);
    println!("=============================================");

    let target = (connect_ok as f64) * (rate_hz as f64);
    let ratio = if target > 0.0 { send_rate / target } else { 0.0 };

    if connect_ok >= (num_clients as f64 * 0.95) as usize && ratio >= 0.90 {
        println!(" RESULT: PASS ({:.1}% of target rate, {:.1}% delivery)",
            ratio * 100.0, delivery);
    } else {
        println!(" RESULT: DEGRADED ({:.1}% of target rate, {}/{} connected, {:.1}% delivery)",
            ratio * 100.0, connect_ok, num_clients, delivery);
    }
    println!("=============================================");
}
