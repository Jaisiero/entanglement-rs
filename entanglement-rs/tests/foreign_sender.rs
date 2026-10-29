//! Sender filter in the client (C++ `client::poll`).
//!
//! A shard with a stale connection that keeps sending HEARTBEATs (1 s) to the
//! bot's local port (reused by another socket) with high sequence numbers used
//! to bump `m_remote_sequence`; from then on the real server's packets were
//! dropped as duplicates (silent bot, `ack_watchdog`). Measured on 2026-10-27
//! in the P1.5 bench.
//!
//! The test starts an echo server, connects a client, sends it a HEARTBEAT
//! with sequence 1_000_000 from another socket and checks, through the FFI,
//! that the packet is dropped and counted and that the echo still arrives.
use entanglement_rs::{EntClient, EntServer};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

const RELIABLE_CHANNEL: u8 = 2; // channels::RELIABLE

/// Control HEARTBEAT (34-byte header + type) with the given sequence.
fn foreign_heartbeat(sequence: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(35);
    p.extend_from_slice(&0xE7A9u16.to_le_bytes()); // magic
    p.push(1); // version
    p.push(1 << 1); // FLAG_CONTROL
    p.extend_from_slice(&0u16.to_le_bytes()); // shard_id
    p.push(0); // channel_id = CONTROL
    p.push(0); // reserved
    p.extend_from_slice(&sequence.to_le_bytes());
    p.extend_from_slice(&0u64.to_le_bytes()); // ack
    p.extend_from_slice(&0u32.to_le_bytes()); // ack_bitmap
    p.extend_from_slice(&0u32.to_le_bytes()); // channel_sequence
    p.extend_from_slice(&1u16.to_le_bytes()); // payload_size
    p.push(0x05); // CONTROL_HEARTBEAT
    p
}

#[test]
fn client_drops_packets_from_foreign_sender() {
    init_network();
    let port = 47131;
    let server = Arc::new(EntServer::new(port, "127.0.0.1"));
    server.set_verbose(false);
    server.register_default_channels();
    let echo = Arc::downgrade(&server);
    server.set_on_client_data(move |hdr, data, from| {
        if let Some(s) = echo.upgrade() {
            let _ = s.send_to(data, hdr.channel_id, from, 0);
        }
    });
    server.start().expect("server starts");
    let server_loop = {
        let s = server.clone();
        std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(5);
            while Instant::now() < end {
                s.poll(256);
                s.update();
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };

    let client = EntClient::new("127.0.0.1", port);
    client.set_verbose(false);
    // Force the filter on: the A/B env var ENTANGLEMENT_CLIENT_SENDER_FILTER=0
    // can turn the default off, and the test must not depend on the environment.
    client.set_sender_filter(true);
    client.register_default_channels();
    let echoes = Arc::new(AtomicU32::new(0));
    {
        let echoes = echoes.clone();
        client.set_on_data_received(move |_, _| { echoes.fetch_add(1, Ordering::Relaxed); });
    }
    client.connect().expect("connects");

    let impostor = UdpSocket::bind("127.0.0.1:0").unwrap();
    impostor.send_to(&foreign_heartbeat(1_000_000), ("127.0.0.1", client.local_port())).unwrap();
    let settle = Instant::now() + Duration::from_millis(50);
    while Instant::now() < settle {
        client.poll(64);
        std::thread::sleep(Duration::from_millis(1));
    }

    client.send(b"PING", RELIABLE_CHANNEL, 0).expect("sends");
    let deadline = Instant::now() + Duration::from_millis(1500);
    while echoes.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        client.poll(64);
        client.update();
        std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(client.foreign_sender_drops(), 1, "the foreign HEARTBEAT is dropped and counted");
    assert!(echoes.load(Ordering::Relaxed) > 0, "the real server's echo still arrives");
    client.disconnect();
    server_loop.join().unwrap();
    server.stop();
}
