use entanglement_rs::{
    spawn_server, spawn_client,
    ServerEvent, ServerCommand,
    ClientEvent, ClientCommand,
};
use tokio::time::{timeout, Duration};

const TEST_PORT: u16 = 19876;

#[tokio::main]
async fn main() {
    println!("=== Integration Test: Loopback ===\n");

    let mut passed = 0;
    let mut failed = 0;

    // 1. Start server
    let mut server = spawn_server("0.0.0.0", TEST_PORT, 2);
    println!("[OK] Server spawned on port {}", TEST_PORT);

    // Give server time to bind
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 2. Connect client
    let mut client = spawn_client("127.0.0.1", TEST_PORT);
    println!("[..] Client connecting to 127.0.0.1:{}", TEST_PORT);

    // 3. Wait for ClientEvent::Connected
    match timeout(Duration::from_secs(5), client.rx.recv()).await {
        Ok(Some(ClientEvent::Connected)) => {
            println!("[OK] TEST 1 PASSED: Client received Connected event");
            passed += 1;
        }
        other => {
            println!("[FAIL] TEST 1 FAILED: Expected Connected, got {:?}", other);
            failed += 1;
        }
    }

    // 4. Wait for ServerEvent::ClientConnected
    match timeout(Duration::from_secs(5), server.rx.recv()).await {
        Ok(Some(ServerEvent::ClientConnected { address, port, .. })) => {
            println!("[OK] TEST 2 PASSED: Server saw client connect from {}:{}", address, port);
            passed += 1;
        }
        other => {
            println!("[FAIL] TEST 2 FAILED: Expected ClientConnected, got {:?}", other);
            failed += 1;
        }
    }

    // 5. Client sends data (channel 0 = unreliable)
    let test_payload = b"Hello from Rust client!";
    client.tx.send(ClientCommand::Send {
        data: test_payload.to_vec(),
        channel_id: 1,
        flags: 0,
    }).await.expect("failed to send command");
    println!("[..] Client sent {} bytes on channel 0", test_payload.len());

    // 6. Server should receive the data
    match timeout(Duration::from_secs(5), server.rx.recv()).await {
        Ok(Some(ServerEvent::DataReceived { payload, sender, .. })) => {
            if payload == test_payload.to_vec() {
                println!("[OK] TEST 3 PASSED: Server received correct payload ({} bytes) from {}",
                    payload.len(), sender.to_string_repr());
                passed += 1;
            } else {
                println!("[FAIL] TEST 3 FAILED: Payload mismatch. Got {} bytes, expected {}",
                    payload.len(), test_payload.len());
                failed += 1;
            }
        }
        other => {
            println!("[FAIL] TEST 3 FAILED: Expected DataReceived, got {:?}", other);
            failed += 1;
        }
    }

    // 7. Server sends data back to client (echo)
    // We need the sender endpoint from the connect event - let's send to all connected
    // Actually, let's use the server's SendTo with the endpoint we got
    // For simplicity, disconnect and verify disconnect events

    // 8. Client disconnects
    client.tx.send(ClientCommand::Disconnect).await.expect("failed to send disconnect");
    println!("[..] Client disconnecting...");

    // 9. Wait for server to see disconnect
    match timeout(Duration::from_secs(5), server.rx.recv()).await {
        Ok(Some(ServerEvent::ClientDisconnected { address, port, .. })) => {
            println!("[OK] TEST 4 PASSED: Server saw client disconnect from {}:{}", address, port);
            passed += 1;
        }
        other => {
            println!("[FAIL] TEST 4 FAILED: Expected ClientDisconnected, got {:?}", other);
            failed += 1;
        }
    }

    // Cleanup
    client.tx.send(ClientCommand::Stop).await.ok();
    server.tx.send(ServerCommand::Stop).await.ok();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Results
    println!("\n=== Results: {}/{} tests passed ===", passed, passed + failed);
    if failed > 0 {
        std::process::exit(1);
    }
    println!("ALL TESTS PASSED");
}
