use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
    usize,
};
use tokio::{net::UdpSocket, sync::RwLock, time::sleep};

use crate::helpers::*;

struct ClientContext {
    local_socket: Arc<UdpSocket>,
    upstream_addresses: Vec<SocketAddr>,
    active_client: Arc<RwLock<Option<SocketAddr>>>,
    last_server_packet_ts: Arc<AtomicU32>,
    key_bytes: [u8; RND_BUF_SIZE],
    inactivity_timeout_sec: u32,
    upstream_sockets: Arc<RwLock<Vec<Arc<UdpSocket>>>>,
    obfuscation_size: usize,
    pad_size: usize,
}

pub async fn client_main(args: &Args, obfuscation_key: u64) -> std::io::Result<()> {
    let bind_port = args.bind.parse::<u16>().expect("client bind parse failed");
    let bind_addr = SocketAddr::new([0, 0, 0, 0].into(), bind_port);
    let local_socket = Arc::new(UdpSocket::bind(bind_addr).await?);
    println!("Client bound to {bind_addr}");

    let upstream_addresses: Vec<SocketAddr> = parse_addresses(&args.upstream).await;
    if upstream_addresses.is_empty() {
        println!("No valid upstream addresses for client.");
        std::process::exit(1);
    }

    let ctx = Arc::new(ClientContext {
        local_socket,
        upstream_addresses,
        active_client: Arc::new(RwLock::new(None)),
        last_server_packet_ts: Arc::new(AtomicU32::new(now_ts())),
        key_bytes: generate_random_buf(obfuscation_key),
        inactivity_timeout_sec: args.inactivity_timeout_sec,
        upstream_sockets: Arc::new(RwLock::new(vec![])),
        obfuscation_size: args.obfuscation_size,
        pad_size: args.pad_size,
    });

    // local->upstream task
    spawn_local_to_upstream_task(ctx.clone());

    // initially create upstream sockets
    recreate_upstream_sockets(ctx.clone()).await;

    // watch inactivity => recreate upstream
    let watch_ctx = ctx.clone();
    tokio::spawn(async move {
        let check_interval = Duration::from_secs((watch_ctx.inactivity_timeout_sec / 2) as u64);
        loop {
            sleep(check_interval).await;
            if now_ts() - watch_ctx.last_server_packet_ts.load(Ordering::Relaxed)
                > watch_ctx.inactivity_timeout_sec
            {
                println!(
                    "(Client) No server data for {:?}, recreating upstream...",
                    watch_ctx.inactivity_timeout_sec
                );
                recreate_upstream_sockets(watch_ctx.clone()).await;
            }
        }
    });

    println!("Client proxy started. Press Ctrl+C to stop.");
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

fn spawn_local_to_upstream_task(ctx: Arc<ClientContext>) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        let mut i: usize = 0;
        loop {
            let (n, from) = match ctx.local_socket.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(e) => {
                    println!("(Client) Error {e} receiving data from local, continuing...");
                    continue;
                }
            };

            {
                let ac = ctx.active_client.read().await;
                if *ac != Some(from) {
                    drop(ac);
                    let mut ac = ctx.active_client.write().await;
                    *ac = Some(from);
                    println!("(Client) Active client set to {from}");
                }
            }

            obfuscation_in_place(&mut buf[..n], &ctx.key_bytes, ctx.obfuscation_size);

            let upstream_sockets = ctx.upstream_sockets.read().await;
            if upstream_sockets.is_empty() {
                println!("(Client) No upstream sockets available.");
                continue;
            }
            let upstream_sock = &upstream_sockets[i % upstream_sockets.len()];
            let n = pad_package(&mut buf, n, &ctx.key_bytes, ctx.pad_size);
            let _ = upstream_sock.send(&buf[..n]).await;
            i = (i + 1) % usize::MAX;
        }
    });
}

/// Recreate all upstream upstream sockets + read tasks.
async fn recreate_upstream_sockets(ctx: Arc<ClientContext>) {
    println!("(Client) Recreating upstream sockets...");

    // reset last_server_packet
    ctx.last_server_packet_ts.store(now_ts(), Ordering::Relaxed);

    // Create new sockets
    let mut new_sockets = Vec::new();
    for upstream_address in &ctx.upstream_addresses {
        let sock = Arc::new(
            UdpSocket::bind("0.0.0.0:0")
                .await
                .expect("Failed to bind upstream for client"),
        );
        sock.connect(upstream_address)
            .await
            .expect("Failed to connect to upstream for client");
        new_sockets.push(sock);
    }

    // spawn read tasks
    for (i, sock) in new_sockets.iter().enumerate() {
        let sock_clone = sock.clone();
        let ctx_clone = ctx.clone();
        let up_addr = ctx_clone.upstream_addresses[i];

        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                let n = match sock_clone.recv(&mut buf).await {
                    Ok(x) => x,
                    Err(_) => break,
                };

                if n < ctx_clone.pad_size {
                    // invalid packet ignore
                    continue;
                }

                let n = unpad_package(&buf, n, ctx_clone.pad_size);

                // mark last_server_packet
                ctx_clone
                    .last_server_packet_ts
                    .store(now_ts(), Ordering::Relaxed);

                obfuscation_in_place(
                    &mut buf[..n],
                    &ctx_clone.key_bytes,
                    ctx_clone.obfuscation_size,
                );

                // forward to active client
                let ac = ctx_clone.active_client.read().await;
                if let Some(client_addr) = *ac {
                    let _ = ctx_clone.local_socket.send_to(&buf[..n], client_addr).await;
                } else {
                    println!("(Client) Received data from {up_addr}, no active client set.");
                }
            }
        });
    }

    // store new in upstream_sockets
    {
        let mut ep_write = ctx.upstream_sockets.write().await;
        *ep_write = new_sockets;
    }
    println!("(Client) Upstream sockets recreated successfully.");
}
