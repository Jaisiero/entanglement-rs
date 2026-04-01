use std::ffi::CString;
use std::sync::Arc;

use entanglement_sys::*;
use crate::error::{check_err, EntResult};
use crate::server::{EntEndpoint, EntPacketHeader, EntCongestionInfo, EntLostPacketInfo, SpawnConfig};

// ── Low-level client wrapper ──

pub struct EntClient {
    inner: *mut ent_client_t,
    _callbacks: std::sync::Mutex<Vec<Box<dyn std::any::Any + Send + Sync>>>,
}

unsafe impl Send for EntClient {}
unsafe impl Sync for EntClient {}

impl EntClient {
    pub fn new(server_address: &str, server_port: u16) -> Self {
        let c_addr = CString::new(server_address).expect("invalid server address");
        let inner = unsafe { ent_client_create(c_addr.as_ptr(), server_port) };
        assert!(!inner.is_null(), "ent_client_create returned null");
        EntClient { inner, _callbacks: std::sync::Mutex::new(Vec::new()) }
    }


    /// Raw C pointer for direct FFI calls from callbacks.
    pub fn raw_handle(&self) -> *mut ent_client_t { self.inner }

    pub fn connect(&self) -> EntResult<()> {
        check_err(unsafe { ent_client_connect(self.inner) })?;
        Ok(())
    }

    pub fn disconnect(&self) {
        unsafe { ent_client_disconnect(self.inner) }
    }

    pub fn is_connected(&self) -> bool {
        unsafe { ent_client_is_connected(self.inner) != 0 }
    }

    pub fn send(&self, data: &[u8], channel_id: u8, flags: u8) -> EntResult<u32> {
        let mut msg_id: u32 = 0;
        let mut seq: u64 = 0;
        let mut ch_seq: u32 = 0;
        let ret = unsafe {
            ent_client_send(
                self.inner,
                data.as_ptr() as *const std::ffi::c_void,
                data.len(),
                channel_id,
                flags,
                &mut msg_id,
                &mut seq,
                0,
                &mut ch_seq,
            )
        };
        check_err(ret)?;
        Ok(msg_id)
    }

    pub fn poll(&self, max_packets: i32) -> i32 {
        unsafe { ent_client_poll(self.inner, max_packets) }
    }

    pub fn update(&self) -> i32 {
        unsafe { ent_client_update(self.inner) }
    }

    pub fn register_default_channels(&self) {
        unsafe { ent_client_register_default_channels(self.inner) }
    }

    pub fn enable_auto_retransmit(&self) {
        unsafe { ent_client_enable_auto_retransmit(self.inner) }
    }

    pub fn can_send(&self) -> bool {
        unsafe { ent_client_can_send(self.inner) != 0 }
    }

    pub fn set_verbose(&self, verbose: bool) {
        unsafe { ent_client_set_verbose(self.inner, verbose as i32) }
    }

    pub fn local_port(&self) -> u16 {
        unsafe { ent_client_local_port(self.inner) }
    }

    // ── Closure-based callback setters ──

    pub fn set_on_data_received<F>(&self, callback: F)
    where F: Fn(&EntPacketHeader, &[u8]) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(&EntPacketHeader, &[u8]) + Send + Sync>
        ));
        unsafe { ent_client_set_on_data_received(self.inner, Some(cli_data_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_connected<F>(&self, callback: F)
    where F: Fn() + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn() + Send + Sync>
        ));
        unsafe { ent_client_set_on_connected(self.inner, Some(cli_connected_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_disconnected<F>(&self, callback: F)
    where F: Fn() + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn() + Send + Sync>
        ));
        unsafe { ent_client_set_on_disconnected(self.inner, Some(cli_disconnected_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_packet_lost<F>(&self, callback: F)
    where F: Fn(&EntLostPacketInfo) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(&EntLostPacketInfo) + Send + Sync>
        ));
        unsafe { ent_client_set_on_packet_lost(self.inner, Some(cli_lost_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    // ── Additional methods ──

    pub fn congestion(&self) -> EntCongestionInfo {
        unsafe { ent_client_congestion(self.inner) }.into()
    }

    pub fn send_fragment(
        &self, msg_id: u32, frag_idx: u8, frag_count: u8,
        data: &[u8], flags: u8, channel_id: u8,
    ) -> EntResult<()> {
        check_err(unsafe {
            ent_client_send_fragment(
                self.inner, msg_id, frag_idx, frag_count,
                data.as_ptr() as *const std::ffi::c_void, data.len(),
                flags, channel_id,
            )
        })?;
        Ok(())
    }

    pub fn is_fragment_throttled(&self) -> bool {
        unsafe { ent_client_is_fragment_throttled(self.inner) != 0 }
    }

    pub fn set_reassembly_timeout(&self, timeout_us: i64) {
        unsafe { ent_client_set_reassembly_timeout(self.inner, timeout_us) }
    }
    // ── Fragment reassembly callback setters ──

    pub fn set_on_allocate_message<F>(&self, callback: F)
    where F: Fn(EntEndpoint, u32, u8, u8, usize) -> *mut u8 + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, u32, u8, u8, usize) -> *mut u8 + Send + Sync>
        ));
        unsafe { ent_client_set_on_allocate_message(self.inner, Some(cli_alloc_msg_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_message_complete<F>(&self, callback: F)
    where F: Fn(EntEndpoint, u32, u8, *mut u8, usize) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, u32, u8, *mut u8, usize) + Send + Sync>
        ));
        unsafe { ent_client_set_on_message_complete(self.inner, Some(cli_msg_complete_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

    pub fn set_on_message_failed<F>(&self, callback: F)
    where F: Fn(EntEndpoint, u32, u8, *mut u8) + Send + Sync + 'static
    {
        let raw = Box::into_raw(Box::new(
            Box::new(callback) as Box<dyn Fn(EntEndpoint, u32, u8, *mut u8) + Send + Sync>
        ));
        unsafe { ent_client_set_on_message_failed(self.inner, Some(cli_msg_failed_cb), raw as *mut _); }
        self._callbacks.lock().unwrap().push(unsafe { Box::from_raw(raw) });
    }

}

// ── Closure callback trampolines (client) ──

unsafe extern "C" fn cli_data_cb(
    header: *const ent_packet_header, payload: *const u8,
    payload_size: usize, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(&EntPacketHeader, &[u8]) + Send + Sync>);
    let hdr = EntPacketHeader::from(&*header);
    let data = std::slice::from_raw_parts(payload, payload_size);
    cb(&hdr, data);
}

unsafe extern "C" fn cli_connected_cb(user_data: *mut std::ffi::c_void) {
    let cb = &**(user_data as *const Box<dyn Fn() + Send + Sync>);
    cb();
}

unsafe extern "C" fn cli_disconnected_cb(user_data: *mut std::ffi::c_void) {
    let cb = &**(user_data as *const Box<dyn Fn() + Send + Sync>);
    cb();
}

unsafe extern "C" fn cli_lost_cb(
    info: *const ent_lost_packet_info, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(&EntLostPacketInfo) + Send + Sync>);
    let li = EntLostPacketInfo::from(&*info);
    cb(&li);
}



unsafe extern "C" fn cli_alloc_msg_cb(
    sender: ent_endpoint, msg_id: u32, ch_id: u8,
    frag_count: u8, max_size: usize, user_data: *mut std::ffi::c_void,
) -> *mut u8 {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, u32, u8, u8, usize) -> *mut u8 + Send + Sync>);
    cb(sender.into(), msg_id, ch_id, frag_count, max_size)
}

unsafe extern "C" fn cli_msg_complete_cb(
    sender: ent_endpoint, msg_id: u32, ch_id: u8,
    data: *mut u8, total_size: usize, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, u32, u8, *mut u8, usize) + Send + Sync>);
    cb(sender.into(), msg_id, ch_id, data, total_size)
}

unsafe extern "C" fn cli_msg_failed_cb(
    sender: ent_endpoint, msg_id: u32, ch_id: u8,
    app_buffer: *mut u8, _reason: ent_message_fail_reason,
    _recv_count: u8, _frag_count: u8, user_data: *mut std::ffi::c_void,
) {
    let cb = &**(user_data as *const Box<dyn Fn(EntEndpoint, u32, u8, *mut u8) + Send + Sync>);
    cb(sender.into(), msg_id, ch_id, app_buffer)
}

impl Drop for EntClient {
    fn drop(&mut self) {
        unsafe { ent_client_destroy(self.inner) }
    }
}

// ── Async bridge types ──

#[derive(Debug)]
pub enum ClientCommand {
    Send { data: Vec<u8>, channel_id: u8, flags: u8 },
    Disconnect,
    Stop,
}

#[derive(Debug)]
pub enum ClientEvent {
    Connected,
    Disconnected,
    DataReceived { header: EntPacketHeader, payload: Vec<u8> },
}

pub struct EntClientHandle {
    pub tx: tokio::sync::mpsc::Sender<ClientCommand>,
    pub rx: tokio::sync::mpsc::Receiver<ClientEvent>,
}

// ── Trampoline callbacks ──

struct ClientCallbackCtx {
    evt_tx: tokio::sync::mpsc::Sender<ClientEvent>,
}

unsafe extern "C" fn on_connected_trampoline(user_data: *mut std::ffi::c_void) {
    let ctx = &*(user_data as *const ClientCallbackCtx);
    let _ = ctx.evt_tx.blocking_send(ClientEvent::Connected);
}

unsafe extern "C" fn on_disconnected_trampoline(user_data: *mut std::ffi::c_void) {
    let ctx = &*(user_data as *const ClientCallbackCtx);
    let _ = ctx.evt_tx.blocking_send(ClientEvent::Disconnected);
}

unsafe extern "C" fn on_data_received_trampoline(
    header: *const ent_packet_header, payload: *const u8, payload_size: usize,
    user_data: *mut std::ffi::c_void,
) {
    let ctx = &*(user_data as *const ClientCallbackCtx);
    let hdr = EntPacketHeader::from(&*header);
    let data = std::slice::from_raw_parts(payload, payload_size).to_vec();
    let _ = ctx.evt_tx.blocking_send(ClientEvent::DataReceived {
        header: hdr,
        payload: data,
    });
}

// ── Spawn function ──

pub fn spawn_client(
    server_address: &str,
    server_port: u16,
) -> EntClientHandle {
    spawn_client_with_config(server_address, server_port, SpawnConfig::default())
}

pub fn spawn_client_with_config(
    server_address: &str,
    server_port: u16,
    config: SpawnConfig,
) -> EntClientHandle {
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ClientCommand>(config.channel_buffer);
    let (evt_tx, evt_rx) = tokio::sync::mpsc::channel::<ClientEvent>(config.channel_buffer);
    let addr = server_address.to_string();

    std::thread::spawn(move || {
        let _ = &config; // moved into closure
        let client = EntClient::new(&addr, server_port);
        client.register_default_channels();

        let ctx = Arc::new(ClientCallbackCtx { evt_tx });
        let ctx_ptr = Arc::into_raw(ctx.clone()) as *mut std::ffi::c_void;

        unsafe {
            ent_client_set_on_connected(client.inner, Some(on_connected_trampoline), ctx_ptr);
            ent_client_set_on_disconnected(client.inner, Some(on_disconnected_trampoline), ctx_ptr);
            ent_client_set_on_data_received(client.inner, Some(on_data_received_trampoline), ctx_ptr);
        }

        if client.connect().is_err() {
            unsafe { Arc::from_raw(ctx_ptr as *const ClientCallbackCtx); }
            return;
        }

        loop {
            loop {
                match cmd_rx.try_recv() {
                    Ok(ClientCommand::Send { data, channel_id, flags }) => {
                        let _ = client.send(&data, channel_id, flags);
                    }
                    Ok(ClientCommand::Disconnect) => {
                        client.disconnect();
                    }
                    Ok(ClientCommand::Stop) => {
                        client.disconnect();
                        unsafe { Arc::from_raw(ctx_ptr as *const ClientCallbackCtx); }
                        return;
                    }
                    Err(_) => break,
                }
            }

            client.poll(config.poll_batch);
            client.update();
            std::thread::sleep(config.tick_interval);
        }
    });

    EntClientHandle { tx: cmd_tx, rx: evt_rx }
}
