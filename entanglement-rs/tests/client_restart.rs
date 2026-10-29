//! A new client instance on the endpoint of a dead one (C++ `server_worker::handle_control`).
//!
//! Between bench runs the load client is killed and its server-side connections
//! stay alive until they time out. A new bot that gets the same local port used
//! to inherit the stale connection: its CONNECTION_REQUEST was just re-accepted,
//! its packets were dropped as duplicates of the old (higher) sequence numbers
//! and its SessionAuth never reached the game (25 s SessionOpen timeout).
//! Measured on 2026-10-28: 4-7 bots per run.
//!
//! The test plays both instances from one fixed UDP port with raw packets: the
//! first one connects with instance id A and sends data up to sequence 500; the
//! second connects with instance id B and sends data with sequence 2. The server
//! must drop the stale connection (one disconnect, two connects) and deliver the
//! newcomer's data. A legacy request without an id keeps the old connection.
use entanglement_rs::EntServer;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
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

const FLAG_CONTROL: u8 = 1 << 1;
const UNRELIABLE_CHANNEL: u8 = 1; // channels::UNRELIABLE
const CONTROL_CONNECTION_REQUEST: u8 = 0x01;

/// 34-byte header + payload.
fn packet(flags: u8, channel: u8, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(34 + payload.len());
    p.extend_from_slice(&0xE7A9u16.to_le_bytes()); // magic
    p.push(1); // version
    p.push(flags);
    p.extend_from_slice(&0u16.to_le_bytes()); // shard_id
    p.push(channel);
    p.push(0); // reserved
    p.extend_from_slice(&sequence.to_le_bytes());
    p.extend_from_slice(&0u64.to_le_bytes()); // ack
    p.extend_from_slice(&0u32.to_le_bytes()); // ack_bitmap
    p.extend_from_slice(&0u32.to_le_bytes()); // channel_sequence
    p.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    p.extend_from_slice(payload);
    p
}

fn connection_request(sequence: u64, instance: Option<u64>) -> Vec<u8> {
    let mut payload = vec![CONTROL_CONNECTION_REQUEST];
    if let Some(id) = instance {
        payload.extend_from_slice(&id.to_le_bytes());
    }
    packet(FLAG_CONTROL, 0, sequence, &payload)
}

struct Harness {
    server: Arc<EntServer>,
    connects: Arc<AtomicU32>,
    disconnects: Arc<AtomicU32>,
    data: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Harness {
    fn new(port: u16) -> Self {
        let server = Arc::new(EntServer::new(port, "127.0.0.1"));
        server.set_verbose(false);
        server.register_default_channels();
        let connects = Arc::new(AtomicU32::new(0));
        let disconnects = Arc::new(AtomicU32::new(0));
        let data = Arc::new(Mutex::new(Vec::new()));
        {
            let c = connects.clone();
            server.set_on_client_connected(move |_, _, _| { c.fetch_add(1, Ordering::SeqCst); });
            let d = disconnects.clone();
            server.set_on_client_disconnected(move |_, _, _| { d.fetch_add(1, Ordering::SeqCst); });
            let data = data.clone();
            server.set_on_client_data(move |_, bytes, _| { data.lock().unwrap().push(bytes.to_vec()); });
        }
        server.start().expect("server starts");
        Harness { server, connects, disconnects, data }
    }

    /// Poll the server until `done` holds or the deadline passes.
    fn pump_until(&self, ms: u64, done: impl Fn(&Self) -> bool) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end && !done(self) {
            self.server.poll(256);
            self.server.update();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn got(&self, bytes: &[u8]) -> bool {
        self.data.lock().unwrap().iter().any(|d| d == bytes)
    }
}

/// First instance: connect with `instance` and push the remote sequence up to 500.
fn first_instance(h: &Harness, sock: &UdpSocket, port: u16, instance: Option<u64>) {
    sock.send_to(&connection_request(1, instance), ("127.0.0.1", port)).unwrap();
    h.pump_until(500, |h| h.connects.load(Ordering::SeqCst) == 1);
    assert_eq!(h.connects.load(Ordering::SeqCst), 1, "first instance connects");
    for seq in 2..=500u64 {
        sock.send_to(&packet(0, UNRELIABLE_CHANNEL, seq, b"OLD"), ("127.0.0.1", port)).unwrap();
    }
    h.pump_until(500, |h| h.got(b"OLD"));
    assert!(h.got(b"OLD"), "first instance's data is delivered");
}

#[test]
fn new_instance_replaces_stale_connection() {
    init_network();
    let port = 47141;
    let h = Harness::new(port);
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    first_instance(&h, &sock, port, Some(0xA11CE));

    // The "dead" client's port is reused by a new instance with another id.
    sock.send_to(&connection_request(1, Some(0xB0B)), ("127.0.0.1", port)).unwrap();
    sock.send_to(&packet(0, UNRELIABLE_CHANNEL, 2, b"NEW"), ("127.0.0.1", port)).unwrap();
    h.pump_until(1000, |h| h.got(b"NEW"));

    assert_eq!(h.disconnects.load(Ordering::SeqCst), 1, "the stale connection is closed once");
    assert_eq!(h.connects.load(Ordering::SeqCst), 2, "the newcomer gets a fresh connection");
    assert!(h.got(b"NEW"), "the newcomer's data is not dropped as a duplicate");

    // A handshake retry of the same instance (same id) must not reset it again.
    sock.send_to(&connection_request(3, Some(0xB0B)), ("127.0.0.1", port)).unwrap();
    h.pump_until(200, |_| false);
    assert_eq!(h.disconnects.load(Ordering::SeqCst), 1, "a retry with the same id keeps the connection");
    assert_eq!(h.connects.load(Ordering::SeqCst), 2);
    h.server.stop();
}

#[test]
fn legacy_request_without_id_keeps_connection() {
    init_network();
    let port = 47142;
    let h = Harness::new(port);
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    first_instance(&h, &sock, port, None);

    sock.send_to(&connection_request(501, None), ("127.0.0.1", port)).unwrap();
    h.pump_until(200, |_| false);
    assert_eq!(h.disconnects.load(Ordering::SeqCst), 0, "legacy clients keep the old behaviour");
    assert_eq!(h.connects.load(Ordering::SeqCst), 1);
    h.server.stop();
}
