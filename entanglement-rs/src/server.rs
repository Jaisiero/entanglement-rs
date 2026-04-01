use std::ffi::CString;
use std::sync::Arc;

use entanglement_sys::*;
use crate::error::{check_err, EntResult};

// ── Value types ──

#[derive(Debug, Clone, Copy)]
pub struct EntEndpoint {
    pub address: u32,
    pub port: u16,
}

impl EntEndpoint {
    pub fn from_string(ip: &str, port: u16) -> Self {
        let c_ip = CString::new(ip).expect("invalid IP string");
        let raw = unsafe { ent_endpoint_from_string(c_ip.as_ptr(), port) };
        EntEndpoint { address: raw.address, port: raw.port }
    }

    pub fn to_string_repr(&self) -> String {
        let mut buf = [0u8; 16];
        let raw = ent_endpoint { address: self.address, port: self.port };
        unsafe { ent_endpoint_to_string(raw, buf.as_mut_ptr() as *mut i8, 16) };
        let len = buf.iter().position(|&b| b == 0).unwrap_or(16);
        String::from_utf8_lossy(&buf[..len]).into_owned()
    }
}

impl From<ent_endpoint> for EntEndpoint {
    fn from(ep: ent_endpoint) -> Self {
        EntEndpoint { address: ep.address, port: ep.port }
    }
}

impl From<EntEndpoint> for ent_endpoint {
    fn from(ep: EntEndpoint) -> Self {
        ent_endpoint { address: ep.address, port: ep.port }
    }
}

#[derive(Debug, Clone)]
pub struct EntPacketHeader {
    pub magic: u16,
    pub version: u8,
    pub flags: u8,
    pub shard_id: u16,
    pub channel_id: u8,
    pub sequence: u64,
    pub ack: u64,
    pub ack_bitmap: u32,
    pub channel_sequence: u32,
    pub payload_size: u16,
}

impl From<&ent_packet_header> for EntPacketHeader {
    fn from(h: &ent_packet_header) -> Self {
        EntPacketHeader {
            magic: h.magic,
            version: h.version,
            flags: h.flags,
            shard_id: h.shard_id,
            channel_id: h.channel_id,
            sequence: h.sequence,
            ack: h.ack,
            ack_bitmap: h.ack_bitmap,
            channel_sequence: h.channel_sequence,
            payload_size: h.payload_size,
        }
    }
}

// ── Low-level server wrapper ──

pub struct EntServer {
    inner: *mut ent_server_t,
    // prevent dropping callback boxes while server is alive
    _callbacks: Vec<Box<dyn std::any::Any + Send>>,
}

// SAFETY: ent_server_t handles its own thread synchronization internally
unsafe impl Send for EntServer {}
unsafe impl Sync for EntServer {}

impl EntServer {
    pub fn new(port: u16, bind_address: &str) -> Self {
        let c_addr = CString::new(bind_address).expect("invalid bind address");
        let inner = unsafe { ent_server_create(port, c_addr.as_ptr()) };
        assert!(!inner.is_null(), "ent_server_create returned null");
        EntServer { inner, _callbacks: Vec::new() }
    }

    pub fn start(&self) -> EntResult<()> {
        check_err(unsafe { ent_server_start(self.inner) })?;
        Ok(())
    }

    pub fn stop(&self) {
        unsafe { ent_server_stop(self.inner) }
    }

    pub fn is_running(&self) -> bool {
        unsafe { ent_server_is_running(self.inner) != 0 }
    }

    pub fn poll(&self, max_packets: i32) -> i32 {
        unsafe { ent_server_poll(self.inner, max_packets) }
    }

    pub fn update(&self) -> i32 {
        unsafe { ent_server_update(self.inner) }
    }

    pub fn send_to(&self, data: &[u8], channel_id: u8, dest: EntEndpoint, flags: u8) -> EntResult<u32> {
        let mut msg_id: u32 = 0;
        let ret = unsafe {
            ent_server_send_to(
                self.inner,
                data.as_ptr() as *const std::ffi::c_void,
                data.len(),
                channel_id,
                dest.into(),
                flags,
                &mut msg_id,
            )
        };
        check_err(ret)?;
        Ok(msg_id)
    }

    pub fn disconnect_client(&self, key: EntEndpoint) {
        unsafe { ent_server_disconnect_client(self.inner, key.into()) }
    }

    pub fn connection_count(&self) -> usize {
        unsafe { ent_server_connection_count(self.inner) }
    }

    pub fn register_default_channels(&self) {
        unsafe { ent_server_register_default_channels(self.inner) }
    }

    pub fn set_worker_count(&self, count: i32) {
        unsafe { ent_server_set_worker_count(self.inner, count) }
    }

    pub fn set_use_async_io(&self, enabled: bool) {
        unsafe { ent_server_set_use_async_io(self.inner, enabled as i32) }
    }

    pub fn set_socket_count(&self, count: i32) {
        unsafe { ent_server_set_socket_count(self.inner, count) }
    }

    pub fn enable_auto_retransmit(&self) {
        unsafe { ent_server_enable_auto_retransmit(self.inner) }
    }

    pub fn set_verbose(&self, verbose: bool) {
        unsafe { ent_server_set_verbose(self.inner, verbose as i32) }
    }

    pub fn port(&self) -> u16 {
        unsafe { ent_server_port(self.inner) }
    }
}

impl Drop for EntServer {
    fn drop(&mut self) {
        unsafe { ent_server_destroy(self.inner) }
    }
}

// ── Async bridge types ──

#[derive(Debug)]
pub enum ServerCommand {
    SendTo { data: Vec<u8>, channel_id: u8, dest: EntEndpoint, flags: u8 },
    Disconnect { endpoint: EntEndpoint },
    Stop,
}

#[derive(Debug)]
pub enum ServerEvent {
    ClientConnected { endpoint: EntEndpoint, address: String, port: u16 },
    ClientDisconnected { endpoint: EntEndpoint, address: String, port: u16 },
    DataReceived { header: EntPacketHeader, payload: Vec<u8>, sender: EntEndpoint },
}

pub struct EntServerHandle {
    pub tx: tokio::sync::mpsc::Sender<ServerCommand>,
    pub rx: tokio::sync::mpsc::Receiver<ServerEvent>,
}

// ── Trampoline callbacks ──

struct ServerCallbackCtx {
    evt_tx: tokio::sync::mpsc::Sender<ServerEvent>,
}

unsafe extern "C" fn on_client_connected_trampoline(
    key: ent_endpoint, address: *const i8, port: u16, user_data: *mut std::ffi::c_void,
) {
    let ctx = &*(user_data as *const ServerCallbackCtx);
    let addr_str = if address.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(address).to_string_lossy().into_owned()
    };
    let _ = ctx.evt_tx.blocking_send(ServerEvent::ClientConnected {
        endpoint: key.into(),
        address: addr_str,
        port,
    });
}

unsafe extern "C" fn on_client_disconnected_trampoline(
    key: ent_endpoint, address: *const i8, port: u16, user_data: *mut std::ffi::c_void,
) {
    let ctx = &*(user_data as *const ServerCallbackCtx);
    let addr_str = if address.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(address).to_string_lossy().into_owned()
    };
    let _ = ctx.evt_tx.blocking_send(ServerEvent::ClientDisconnected {
        endpoint: key.into(),
        address: addr_str,
        port,
    });
}

unsafe extern "C" fn on_client_data_trampoline(
    header: *const ent_packet_header, payload: *const u8, payload_size: usize,
    sender: ent_endpoint, user_data: *mut std::ffi::c_void,
) {
    let ctx = &*(user_data as *const ServerCallbackCtx);
    let hdr = EntPacketHeader::from(&*header);
    let data = std::slice::from_raw_parts(payload, payload_size).to_vec();
    let _ = ctx.evt_tx.blocking_send(ServerEvent::DataReceived {
        header: hdr,
        payload: data,
        sender: sender.into(),
    });
}

// ── Spawn function ──

pub fn spawn_server(
    bind_address: &str,
    port: u16,
    worker_count: i32,
) -> EntServerHandle {
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ServerCommand>(1024);
    let (evt_tx, evt_rx) = tokio::sync::mpsc::channel::<ServerEvent>(1024);
    let bind_addr = bind_address.to_string();

    std::thread::spawn(move || {
        let server = EntServer::new(port, &bind_addr);
        server.set_worker_count(worker_count);
        server.register_default_channels();

        // Callback context leaked intentionally — lives as long as the thread
        let ctx = Arc::new(ServerCallbackCtx { evt_tx });
        let ctx_ptr = Arc::into_raw(ctx.clone()) as *mut std::ffi::c_void;

        unsafe {
            ent_server_set_on_client_connected(
                server.inner, Some(on_client_connected_trampoline), ctx_ptr,
            );
            ent_server_set_on_client_disconnected(
                server.inner, Some(on_client_disconnected_trampoline), ctx_ptr,
            );
            ent_server_set_on_client_data(
                server.inner, Some(on_client_data_trampoline), ctx_ptr,
            );
        }

        server.start().expect("Failed to start server");

        loop {
            // Drain commands (non-blocking)
            loop {
                match cmd_rx.try_recv() {
                    Ok(ServerCommand::SendTo { data, channel_id, dest, flags }) => {
                        let _ = server.send_to(&data, channel_id, dest, flags);
                    }
                    Ok(ServerCommand::Disconnect { endpoint }) => {
                        server.disconnect_client(endpoint);
                    }
                    Ok(ServerCommand::Stop) => {
                        server.stop();
                        // Clean up the Arc ref we leaked
                        unsafe { Arc::from_raw(ctx_ptr as *const ServerCallbackCtx); }
                        return;
                    }
                    Err(_) => break,
                }
            }

            server.poll(256);
            server.update();
            std::thread::sleep(std::time::Duration::from_millis(33)); // ~30Hz
        }
    });

    EntServerHandle { tx: cmd_tx, rx: evt_rx }
}