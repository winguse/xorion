use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{net::UdpSocket, time::timeout};

use crate::helpers::*;

struct InBoundInfo {
    outbound_sock: UdpSocket,
    last_active_ts: AtomicU32,
}

async fn spawn_outbound_reader(
    info: Arc<InBoundInfo>,
    inbound_sock: Arc<UdpSocket>,
    addr: SocketAddr,
    pad_size: usize,
    obfuscation_size: usize,
    key_bytes: Arc<[u8; RND_BUF_SIZE]>,
    is_client: bool,
    inactivity_timeout_sec: u32,
) {
    tokio::spawn(async move {
        let mut buf = vec![0; 65535];
        loop {
            let (mut n, _) = match timeout(
                Duration::from_secs(inactivity_timeout_sec as u64),
                info.outbound_sock.recv_from(&mut buf),
            )
            .await
            {
                Ok(Ok(x)) => x,
                _ => {
                    println!(
                        "Timeout for outbound reader, removing from outbound_map for {}",
                        addr
                    );
                    break;
                }
            };

            // Process the buffer based on client or server mode
            if is_client {
                n = unpad_package(&buf, n, pad_size);
                obfuscation_in_place(&mut buf[..n], &key_bytes, obfuscation_size);
            } else {
                obfuscation_in_place(&mut buf[..n], &key_bytes, obfuscation_size);
                n = pad_package(&mut buf, n, &key_bytes, pad_size);
            }

            // Update the last active timestamp
            info.last_active_ts.store(now_ts(), Ordering::Relaxed);

            // Send the processed buffer to the inbound socket
            let _ = inbound_sock.send_to(&buf[..n], &addr).await;
        }
    });
}

pub async fn singular_main(args: &Args, obfuscation_key: u64) {
    let ports: Vec<u16> = args
        .bind
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if ports.len() > 1 {
        println!("Exactly 1 port in --singular, only use {}", ports[0]);
    }
    let bind_addr = SocketAddr::new([0, 0, 0, 0].into(), ports[0]);
    let inbound_sock = Arc::new(
        UdpSocket::bind(bind_addr)
            .await
            .expect("singular bind failed"),
    );
    println!("Singular bound to {bind_addr}");

    let key_bytes = Arc::new(generate_random_buf(obfuscation_key));

    let mut buf = vec![0; 65535];
    let mut outbound_map: HashMap<SocketAddr, Arc<InBoundInfo>> = HashMap::new();
    let mut last_gc_ts = now_ts();

    loop {
        let now = now_ts();

        // Garbage collect inactive connections
        if now - last_gc_ts > args.inactivity_timeout_sec {
            outbound_map.retain(|_, info| {
                now - info.last_active_ts.load(Ordering::Relaxed) <= args.inactivity_timeout_sec
            });
            last_gc_ts = now;
        }

        // Receive data from the inbound socket
        let (mut n, addr) = match timeout(
            Duration::from_secs(args.inactivity_timeout_sec as u64),
            inbound_sock.recv_from(&mut buf),
        )
        .await
        {
            Ok(Ok(x)) => x,
            _ => continue,
        };

        let info = match outbound_map.get(&addr) {
            Some(info) => {
                info.last_active_ts.store(now, Ordering::Relaxed);
                info.clone()
            }
            None => {
                let upstream_addresses = parse_addresses(&args.upstream).await;
                if upstream_addresses.is_empty() {
                    println!("No valid upstream addresses for client.");
                    continue;
                }
                if upstream_addresses.len() > 1 {
                    println!(
                        "Exactly 1 upstream address in --singular, only use {}",
                        upstream_addresses[0]
                    );
                }
                match UdpSocket::bind("0.0.0.0:0").await {
                    Ok(outbound_sock) => {
                        let _ = outbound_sock.connect(upstream_addresses[0]).await;
                        let info = Arc::new(InBoundInfo {
                            outbound_sock,
                            last_active_ts: AtomicU32::new(now),
                        });
                        outbound_map.insert(addr, info.clone());
                        spawn_outbound_reader(
                            info.clone(),
                            inbound_sock.clone(),
                            addr,
                            args.pad_size,
                            args.obfuscation_size,
                            key_bytes.clone(),
                            args.client,
                            args.inactivity_timeout_sec,
                        )
                        .await;
                        info
                    }
                    Err(_) => {
                        println!("Failed to bind outbound socket for {}", addr);
                        continue;
                    }
                }
            }
        };

        // Process the buffer based on client or server mode
        if args.client {
            obfuscation_in_place(&mut buf[..n], &key_bytes, args.obfuscation_size);
            n = pad_package(&mut buf, n, &key_bytes, args.pad_size);
        } else {
            n = unpad_package(&buf, n, args.pad_size);
            obfuscation_in_place(&mut buf[..n], &key_bytes, args.obfuscation_size);
        }

        // Send the processed buffer to the outbound socket
        if let Err(e) = info.outbound_sock.send(&buf[..n]).await {
            println!(
                "Failed to send to upstream: {}, removing from outbound_map for {}",
                e, addr
            );
            outbound_map.remove(&addr);
        }
    }
}
