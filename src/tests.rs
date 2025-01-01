use std::{net::SocketAddr, time::Duration};

use super::*;
use crate::helpers::*;
use portpicker::pick_unused_port;
use rand::{rngs::StdRng, RngCore, SeedableRng};
use tokio::{
    net::UdpSocket,
    time::{sleep, timeout},
};

const TEST_PAD_SIZE: usize = 30;
const TEST_OBFUSCATION_SIZE: usize = 20;

#[test]
fn test_obfuscation_in_place() {
    let mut key_bytes = [0xFF; RND_BUF_SIZE];
    for i in 0..RND_BUF_SIZE {
        key_bytes[i] = (i + 1) as u8;
    }

    for i in 1..20 {
        let test_size = i * i * 3;
        let mut buf = vec![0; test_size];
        for j in 0..test_size {
            buf[j] = j as u8;
        }

        obfuscation_in_place(&mut buf, &key_bytes, TEST_OBFUSCATION_SIZE);
        let mut is_same = true;
        for j in 0..test_size {
            is_same = is_same && buf[j] == (j as u8);
        }
        assert!(!is_same, "buf after: {:?}", buf);

        obfuscation_in_place(&mut buf, &key_bytes, TEST_OBFUSCATION_SIZE);
        let mut is_same = true;
        for j in 0..test_size {
            is_same = is_same && buf[j] == (j as u8);
        }
        assert!(is_same, "buf again: {:?}", buf);
    }
}

#[test]
fn test_pad_package() {
    let mut buf = vec![0u8; 65535];
    let key_bytes = [1u8; RND_BUF_SIZE];
    let package = b"Hello, world!";
    buf[..package.len()].copy_from_slice(package);

    let padded_size = pad_package(&mut buf, package.len(), &key_bytes, TEST_PAD_SIZE);
    assert_eq!(padded_size, TEST_PAD_SIZE);
    assert_eq!(
        buf[package.len()..TEST_PAD_SIZE - 1],
        key_bytes[..TEST_PAD_SIZE - 1 - package.len()]
    );
    assert_eq!(buf[TEST_PAD_SIZE - 1], package.len() as u8);
}

#[test]
fn test_unpad_package() {
    let mut buf = vec![0u8; 65535];
    let key_bytes = [1u8; RND_BUF_SIZE];
    let package = b"Hello, world!";
    buf[..package.len()].copy_from_slice(package);

    let padded_size = pad_package(&mut buf, package.len(), &key_bytes, TEST_PAD_SIZE);
    let unpadded_size = unpad_package(&mut buf, padded_size, TEST_PAD_SIZE);
    assert_eq!(unpadded_size, package.len());
    assert_eq!(&buf[..unpadded_size], package);
}

#[test]
fn test_pad_package_larger_than_pad_size() {
    let mut buf = vec![0u8; 65535];
    let key_bytes = [1u8; RND_BUF_SIZE];
    let package = vec![0u8; TEST_PAD_SIZE + 10];
    buf[..package.len()].copy_from_slice(&package);

    let padded_size = pad_package(&mut buf, package.len(), &key_bytes, TEST_PAD_SIZE);
    assert_eq!(padded_size, TEST_PAD_SIZE + 10);
    assert_eq!(&buf[..padded_size], &package);
}

#[test]
fn test_pad_unpad() {
    let mut key_bytes = [0xFF; RND_BUF_SIZE];
    for i in 0..RND_BUF_SIZE {
        key_bytes[i] = (i + 1) as u8;
    }

    let mut buf = vec![0u8; 65535];
    for i in 1..20 {
        let test_size = i * i * 3;
        for j in 0..test_size {
            buf[j] = j as u8;
        }
        let pad_size = pad_package(&mut buf, test_size, &key_bytes, TEST_PAD_SIZE);
        assert_eq!(pad_size, TEST_PAD_SIZE.max(test_size));
        let unpad_size = unpad_package(&mut buf, pad_size, TEST_PAD_SIZE);
        assert_eq!(unpad_size, test_size);
        for j in 0..test_size {
            assert_eq!(buf[j], j as u8);
        }
    }
}

#[test]
fn test_parse_obfuscation_key_hex() {
    let parsed = parse_obfuscation_key("0x7f8c1a44");
    assert_eq!(parsed, 0x7F8C1A44);
}

#[test]
fn test_parse_obfuscation_key_decimal() {
    let parsed = parse_obfuscation_key("123456");
    assert_eq!(parsed, 123456);
}

#[test]
fn test_parse_obfuscation_key_invalid() {
    let parsed = parse_obfuscation_key("notanumber");
    assert_eq!(parsed, 0);
}

#[test]
fn test_generate_random_buf() {
    let buf = generate_random_buf(0x12345678);
    assert_eq!(buf[0], 161);
    assert_eq!(buf[100], 94);
    assert_eq!(buf[500], 132);
    assert_eq!(buf[1000], 19);
    assert_eq!(buf[2000], 167);
    assert_eq!(buf[4000], 222);
}

#[tokio::test]
async fn test_parse_addresses() {
    let addr = parse_addresses("127.0.0.1:1234,1235").await;
    assert_eq!(addr.len(), 2);
    let addr = parse_addresses("github.com:1234,1235").await;
    assert_eq!(addr.len(), 2);
}

/// Integration test: starts a "fake server upstream," then runs server & client, verifies data flow.
#[tokio::test]
async fn test_full_e2e() -> std::io::Result<()> {
    test_full_e2e_internal(false).await
}
#[tokio::test]
async fn test_full_e2e_singular() -> std::io::Result<()> {
    test_full_e2e_internal(true).await
}

async fn test_full_e2e_internal(singular: bool) -> std::io::Result<()> {
    let server_inbound_port1 = pick_unused_port().unwrap();
    let server_inbound_port2 = pick_unused_port().unwrap();
    let server_upstream_port = pick_unused_port().unwrap();
    let client_bind_port = pick_unused_port().unwrap();

    // Fake server upstream
    let fake_server_upstream = UdpSocket::bind(("127.0.0.1", server_upstream_port)).await?;
    println!(
        "Fake server upstream bound at port {}",
        fake_server_upstream.local_addr().unwrap()
    );

    // Spawn real server
    let server_args = Args {
        server: true,
        client: false,
        singular,
        bind: format!("{},{}", server_inbound_port1, server_inbound_port2),
        upstream: format!("127.0.0.1:{}", server_upstream_port),
        obfuscation_key: "0x12345678".into(),
        inactivity_timeout_sec: 2,
        obfuscation_size: TEST_OBFUSCATION_SIZE,
        pad_size: TEST_PAD_SIZE,
    };
    let _server_handle = tokio::spawn(async move {
        let _ = server::server_main(
            &server_args,
            parse_obfuscation_key(&server_args.obfuscation_key),
        )
        .await;
    });

    // Spawn real client
    let client_args = Args {
        server: false,
        client: true,
        singular,
        bind: format!("{}", client_bind_port),
        upstream: format!(
            "127.0.0.1:{},{}",
            server_inbound_port1, server_inbound_port2
        ),
        obfuscation_key: "0x12345678".into(),
        inactivity_timeout_sec: 2,
        obfuscation_size: TEST_OBFUSCATION_SIZE,
        pad_size: TEST_PAD_SIZE,
    };
    let _client_handle = tokio::spawn(async move {
        let _ = client::client_main(
            &client_args,
            parse_obfuscation_key(&client_args.obfuscation_key),
        )
        .await;
    });

    // Local "fake client"
    let fake_local_client = UdpSocket::bind("127.0.0.1:0").await?;
    let client_addr = SocketAddr::new("127.0.0.1".parse().unwrap(), client_bind_port);

    // sleep for a bit to let the server and client start
    sleep(Duration::from_secs(1)).await;
    println!(
        "Fake local client bound at port {}",
        fake_local_client.local_addr().unwrap()
    );

    let mut upstream_from: Option<SocketAddr> = None;

    for i in 0..10 {
        // 1) Send local -> client -> server -> upstream
        let msg1 = format!("HELLO_FROM_LOCAL {}", i);
        let msg1 = msg1.as_bytes();
        fake_local_client.send_to(msg1, client_addr).await?;
        let mut buf = [0u8; 1024];

        let (n, from) = timeout(
            Duration::from_secs(3),
            fake_server_upstream.recv_from(&mut buf),
        )
        .await
        .expect("Timed out waiting for data at fake_server_upstream")
        .expect("Error reading fake_server_upstream");

        assert_eq!(&buf[..n], msg1, "Server->upstream mismatch");
        println!(
            "Got {:?} from server at fake upstream, from {:?}",
            &buf[..n],
            from
        );
        upstream_from = Some(from);

        // 2) Send upstream -> server -> client -> local
        let msg2 = format!("HELLO_FROM_UPSTREAM {i}");
        let msg2 = msg2.as_bytes();
        fake_server_upstream.send_to(msg2, from).await?;
        let (n2, from2) = timeout(
            Duration::from_secs(3),
            fake_local_client.recv_from(&mut buf),
        )
        .await
        .expect("Timed out waiting for data at fake_local_client")
        .expect("Error reading fake_local_client");

        assert_eq!(&buf[..n2], msg2, "Upstream->client->local mismatch");
        println!(
            "Got {:?} from client at fake local, from {:?}",
            &buf[..n2],
            from2
        );
    }

    {
        // sleep 3 seconds to let the inactivity server timeout kick in
        sleep(Duration::from_secs(4)).await;
        let msg1 = b"HELLO_FROM_LOCAL SERVER TIMEOUT";
        fake_local_client.send_to(msg1, client_addr).await?;
        let mut buf = [0u8; 1024];

        let (n, from) = timeout(
            Duration::from_secs(3),
            fake_server_upstream.recv_from(&mut buf),
        )
        .await
        .expect("Timed out waiting for data at fake_server_upstream")
        .expect("Error reading fake_server_upstream");

        assert_eq!(&buf[..n], msg1, "Server->upstream mismatch");
        println!(
            "Got {:?} from server at fake upstream, from {:?}",
            &buf[..n],
            from
        );
        assert_ne!(upstream_from, Some(from), "Server->upstream mismatch");

        // also test read from server again
        let msg2 = format!("HELLO_FROM_UPSTREAM TIMEOUT Test");
        let msg2 = msg2.as_bytes();
        fake_server_upstream.send_to(msg2, from).await?;
        let (n2, from2) = timeout(
            Duration::from_secs(3),
            fake_local_client.recv_from(&mut buf),
        )
        .await
        .expect("Timed out waiting for data at fake_local_client")
        .expect("Error reading fake_local_client");

        assert_eq!(&buf[..n2], msg2, "Upstream->client->local mismatch");
        println!(
            "Got {:?} from client at fake local, from {:?}",
            &buf[..n2],
            from2
        );
    }

    Ok(())
}

pub async fn profiling(singular: bool) {
    let server_inbound_port1 = pick_unused_port().unwrap();
    let server_inbound_port2 = pick_unused_port().unwrap();
    let server_upstream_port = pick_unused_port().unwrap();
    let client_bind_port = pick_unused_port().unwrap();

    // Fake server upstream
    let fake_server_upstream = UdpSocket::bind(("127.0.0.1", server_upstream_port))
        .await
        .unwrap();
    println!(
        "Fake server upstream bound at port {}",
        fake_server_upstream.local_addr().unwrap()
    );

    // Spawn real server
    let server_args = Args {
        server: true,
        client: false,
        singular,
        bind: format!("{},{}", server_inbound_port1, server_inbound_port2),
        upstream: format!("127.0.0.1:{}", server_upstream_port),
        obfuscation_key: "0x12345678".into(),
        inactivity_timeout_sec: 2,
        obfuscation_size: TEST_OBFUSCATION_SIZE,
        pad_size: TEST_PAD_SIZE,
    };
    let _server_handle = tokio::spawn(async move {
        let _ = server::server_main(
            &server_args,
            parse_obfuscation_key(&server_args.obfuscation_key),
        )
        .await;
    });

    // Spawn real client
    let client_args = Args {
        server: false,
        client: true,
        singular,
        bind: format!("{}", client_bind_port),
        upstream: format!(
            "127.0.0.1:{},{}",
            server_inbound_port1, server_inbound_port2
        ),
        obfuscation_key: "0x12345678".into(),
        inactivity_timeout_sec: 2,
        obfuscation_size: TEST_OBFUSCATION_SIZE,
        pad_size: TEST_PAD_SIZE,
    };
    let _client_handle = tokio::spawn(async move {
        let _ = client::client_main(
            &client_args,
            parse_obfuscation_key(&client_args.obfuscation_key),
        )
        .await;
    });

    // Local "fake client"
    let fake_local_client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let client_addr = SocketAddr::new("127.0.0.1".parse().unwrap(), client_bind_port);

    // sleep for a bit to let the server and client start
    sleep(Duration::from_secs(1)).await;

    let mut message_bytes = [0u8; 1420];

    StdRng::seed_from_u64(0u64).fill_bytes(&mut message_bytes);

    let mut buf = [0u8; 65535];

    for _ in 0..1000000 {
        // 1) Send local -> client -> server -> upstream
        fake_local_client
            .send_to(&message_bytes, client_addr)
            .await
            .unwrap();

        let (_, from) = timeout(
            Duration::from_secs(3),
            fake_server_upstream.recv_from(&mut buf),
        )
        .await
        .expect("Timed out waiting for data at fake_server_upstream")
        .expect("Error reading fake_server_upstream");

        // 2) Send upstream -> server -> client -> local
        fake_server_upstream
            .send_to(&message_bytes, from)
            .await
            .unwrap();
        let (_, _) = timeout(
            Duration::from_secs(3),
            fake_local_client.recv_from(&mut buf),
        )
        .await
        .expect("Timed out waiting for data at fake_local_client")
        .expect("Error reading fake_local_client");
    }
}
