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


// ── Congestion & loss types ──

#[derive(Debug, Clone, Copy)]
pub struct EntCongestionInfo {
    pub cwnd: u32,
    pub in_flight: u32,
    pub ssthresh: u32,
    pub pacing_interval_us: i64,
    pub in_slow_start: bool,
}

impl From<ent_congestion_info> for EntCongestionInfo {
    fn from(c: ent_congestion_info) -> Self {
        EntCongestionInfo {
            cwnd: c.cwnd, in_flight: c.in_flight, ssthresh: c.ssthresh,
            pacing_interval_us: c.pacing_interval_us, in_slow_start: c.in_slow_start != 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EntLostPacketInfo {
    pub sequence: u64,
    pub flags: u8,
    pub channel_id: u8,
    pub shard_id: u16,
    pub payload_size: u16,
    pub channel_sequence: u32,
    pub message_id: u32,
    pub fragment_index: u8,
    pub fragment_count: u8,
}

impl From<&ent_lost_packet_info> for EntLostPacketInfo {
    fn from(i: &ent_lost_packet_info) -> Self {
        EntLostPacketInfo {
            sequence: i.sequence, flags: i.flags, channel_id: i.channel_id,
            shard_id: i.shard_id, payload_size: i.payload_size,
            channel_sequence: i.channel_sequence, message_id: i.message_id,
            fragment_index: i.fragment_index, fragment_count: i.fragment_count,
        }
    }
}

// ── Low-level server wrapper ──

pub struct EntServer {
    inner: *mut ent_server_t,
    // prevent dropping callback boxes while server is alive
    _callbacks: std::sync::Mutex<Vec<Box<dyn std::any::Any + Send + Sync>>>,
}

// SAFETY: ent_server_t handles its own thread synchronization internally
unsafe impl Send for EntServer {}
unsafe impl Sync for EntServer {}

impl EntServer {
    pub fn new(port: u16, bind_address: &str) -> Self {
        let c_addr = CString::new(bind_address).expect("invalid bind address");
        let inner = unsafe { ent_server_create(port, c_addr.as_ptr()) };
        assert!(!inner.is_null(), "ent_server_create returned null");
        EntServer { inner, _callbacks: std::sync::Mutex::new(Vec::new()) }
    }


    /// Raw C pointer for direct FFI calls from callbacks.
    /// SAFETY: valid as long as this EntServer is alive.
    pub fn raw_handle(&self) -> *mut ent_server_t { self.inner }

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

    // ── Closure-based callback setters ──

    pub fn set_on_client_data<F>(&self, callback: F)
    where F: Fn(&EntPacketHeader, &[u8], EntEndpoint) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(&EntPacketHeader, &[u8], EntEndpoint) + Send + Sync>
        ));
        unsafe { ent_server_set_on_client_data(self.inner, Some(srv_data_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_client_connected<F>(&self, callback: F)
    where F: Fn(EntEndpoint, &str, u16) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, &str, u16) + Send + Sync>
        ));
        unsafe { ent_server_set_on_client_connected(self.inner, Some(srv_connected_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_client_disconnected<F>(&self, callback: F)
    where F: Fn(EntEndpoint, &str, u16) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, &str, u16) + Send + Sync>
        ));
        unsafe { ent_server_set_on_client_disconnected(self.inner, Some(srv_disconnected_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_packet_lost<F>(&self, callback: F)
    where F: Fn(&EntLostPacketInfo, EntEndpoint) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(&EntLostPacketInfo, EntEndpoint) + Send + Sync>
        ));
        unsafe { ent_server_set_on_packet_lost(self.inner, Some(srv_lost_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    // ── Additional methods ──

    pub fn send_fragment_to(
        &self, msg_id: u32, frag_idx: u8, frag_count: u8,
        data: &[u8], flags: u8, channel_id: u8, dest: EntEndpoint, ch_seq: u32,
    ) -> EntResult<()> {
        check_err(unsafe {
            ent_server_send_fragment_to(
                self.inner, msg_id, frag_idx, frag_count,
                data.as_ptr() as *const std::ffi::c_void, data.len(),
                flags, channel_id, dest.into(), ch_seq,
            )
        })?;
        Ok(())
    }

    pub fn recv_queue_drops(&self) -> u64 {
        unsafe { ent_server_recv_queue_drops(self.inner) }
    }

    pub fn is_fragment_throttled(&self, dest: EntEndpoint) -> bool {
        unsafe { ent_server_is_fragment_throttled(self.inner, dest.into()) != 0 }
    }

    pub fn set_reassembly_timeout(&self, timeout_us: i64) {
        unsafe { ent_server_set_reassembly_timeout(self.inner, timeout_us) }
    }
    // ── Fragment reassembly callback setters ──

    pub fn set_on_allocate_message<F>(&self, callback: F)
    where F: Fn(EntEndpoint, u32, u8, u8, usize) -> *mut u8 + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, u32, u8, u8, usize) -> *mut u8 + Send + Sync>
        ));
        unsafe { ent_server_set_on_allocate_message(self.inner, Some(srv_alloc_msg_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_message_complete<F>(&self, callback: F)
    where F: Fn(EntEndpoint, u32, u8, *mut u8, usize) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, u32, u8, *mut u8, usize) + Send + Sync>
        ));
        unsafe { ent_server_set_on_message_complete(self.inner, Some(srv_msg_complete_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_message_failed<F>(&self, callback: F)
    where F: Fn(EntEndpoint, u32, u8, *mut u8) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, u32, u8, *mut u8) + Send + Sync>
        ));
        unsafe { ent_server_set_on_message_failed(self.inner, Some(srv_msg_failed_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

}

// ── Closure callback trampolines (server) ──

unsafe extern "C" fn srv_data_cb(
    header: *const ent_packet_header, payload: *const u8,
    payload_size: usize, sender: ent_endpoint, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(&EntPacketHeader, &[u8], EntEndpoint) + Send + Sync>);
    let hdr = EntPacketHeader::from(&*header);
    let data = std::slice::from_raw_parts(payload, payload_size);
    cb(&hdr, data, sender.into());
}

unsafe extern "C" fn srv_connected_cb(
    key: ent_endpoint, address: *const i8, port: u16, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, &str, u16) + Send + Sync>);
    let addr = if address.is_null() { "" }
               else { std::ffi::CStr::from_ptr(address).to_str().unwrap_or("") };
    cb(key.into(), addr, port);
}

unsafe extern "C" fn srv_disconnected_cb(
    key: ent_endpoint, address: *const i8, port: u16, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, &str, u16) + Send + Sync>);
    let addr = if address.is_null() { "" }
               else { std::ffi::CStr::from_ptr(address).to_str().unwrap_or("") };
    cb(key.into(), addr, port);
}

unsafe extern "C" fn srv_lost_cb(
    info: *const ent_lost_packet_info, client: ent_endpoint, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(&EntLostPacketInfo, EntEndpoint) + Send + Sync>);
    let li = EntLostPacketInfo::from(&*info);
    cb(&li, client.into());
}



unsafe extern "C" fn srv_alloc_msg_cb(
    sender: ent_endpoint, msg_id: u32, ch_id: u8,
    frag_count: u8, max_size: usize, user_data: *mut std::ffi::c_void,
) -> *mut u8 {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, u32, u8, u8, usize) -> *mut u8 + Send + Sync>);
    cb(sender.into(), msg_id, ch_id, frag_count, max_size)
}

unsafe extern "C" fn srv_msg_complete_cb(
    sender: ent_endpoint, msg_id: u32, ch_id: u8,
    data: *mut u8, total_size: usize, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, u32, u8, *mut u8, usize) + Send + Sync>);
    cb(sender.into(), msg_id, ch_id, data, total_size)
}

unsafe extern "C" fn srv_msg_failed_cb(
    sender: ent_endpoint, msg_id: u32, ch_id: u8,
    app_buffer: *mut u8, _reason: ent_message_fail_reason,
    _recv_count: u8, _frag_count: u8, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, u32, u8, *mut u8) + Send + Sync>);
    cb(sender.into(), msg_id, ch_id, app_buffer)
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


// ── Spawn configuration ──

/// Configuration for the async bridge tick loop and channel buffers.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    /// Capacity of the tokio mpsc channels (default: 1024).
    pub channel_buffer: usize,
    /// Sleep duration between tick iterations (default: 33ms = ~30Hz).
    pub tick_interval: std::time::Duration,
    /// Maximum packets to process per poll call (default: 256).
    pub poll_batch: i32,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        SpawnConfig {
            channel_buffer: 1024,
            tick_interval: std::time::Duration::from_millis(33),
            poll_batch: 256,
        }
    }
}

impl SpawnConfig {
    /// High-throughput settings for benchmarks and soak tests.
    pub fn performance() -> Self {
        SpawnConfig {
            channel_buffer: 65536,
            tick_interval: std::time::Duration::from_millis(1),
            poll_batch: 4096,
        }
    }
}

// ── Spawn function ──

pub fn spawn_server(
    bind_address: &str,
    port: u16,
    worker_count: i32,
) -> EntServerHandle {
    spawn_server_with_config(bind_address, port, worker_count, SpawnConfig::default())
}

pub fn spawn_server_with_config(
    bind_address: &str,
    port: u16,
    worker_count: i32,
    config: SpawnConfig,
) -> EntServerHandle {
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ServerCommand>(config.channel_buffer);
    let (evt_tx, evt_rx) = tokio::sync::mpsc::channel::<ServerEvent>(config.channel_buffer);
    let bind_addr = bind_address.to_string();

    std::thread::spawn(move || {
        let _ = &config; // moved into closure
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

            server.poll(config.poll_batch);
            server.update();
            std::thread::sleep(config.tick_interval);
        }
    });

    EntServerHandle { tx: cmd_tx, rx: evt_rx }
}