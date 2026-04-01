use entanglement_rs::{spawn_server, ServerEvent};

#[tokio::main]
async fn main() {
    let handle = spawn_server("0.0.0.0", 9876, 4);
    println!("Server started on port 9876");

    let mut rx = handle.rx;
    while let Some(event) = rx.recv().await {
        match event {
            ServerEvent::ClientConnected { address, port, .. } => {
                println!("Client connected: {}:{}", address, port);
            }
            ServerEvent::ClientDisconnected { address, port, .. } => {
                println!("Client disconnected: {}:{}", address, port);
            }
            ServerEvent::DataReceived { payload, sender, .. } => {
                println!("Data from {:?}: {} bytes", sender.to_string_repr(), payload.len());
            }
        }
    }
}
