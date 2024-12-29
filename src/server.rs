use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Weak,
    },
    time::Duration,
    usize,
};
use tokio::{
    net::UdpSocket,
    sync::{Mutex, RwLock},
    time::sleep,
};

use crate::helpers::*;

struct ConnectionInfo {
    upstream_socket: Arc<UdpSocket>,
    recent_addresses: RwLock<Vec<Option<SocketAddr>>>,
    last_client_packet_ts: Arc<AtomicU32>,
    upstream_read_loop_exit: bool,
}

struct ServerContext {
    inbound_sockets: Vec<Arc<UdpSocket>>,
    connection_map: Mutex<HashMap<IpAddr, Arc<RwLock<ConnectionInfo>>>>,
    upstream: String,
    key_bytes: [u8; RND_BUF_SIZE],
    inactivity_timeout_sec: u32,
    obfuscation_size: usize,
    pad_size: usize,
}

pub async fn server_main(args: &Args, obfuscation_key: u64) -> std::io::Result<()> {
    // Parse server listening ports
    let ports: Vec<u16> = args
        .bind
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if ports.is_empty() {
        println!("No valid ports in --bind for server.");
        std::process::exit(1);
    }

    // Create inbound sockets
    let mut inbound_sockets = Vec::new();
    for port in ports {
        let bind_addr = SocketAddr::new([0, 0, 0, 0].into(), port);
        let sock = UdpSocket::bind(bind_addr).await?;
        println!("Server bound to {bind_addr}");
        inbound_sockets.push(Arc::new(sock));
    }

    let ctx = Arc::new(ServerContext {
        inbound_sockets,
        connection_map: Mutex::new(HashMap::new()),
        upstream: args.upstream.clone(),
        key_bytes: generate_random_buf(obfuscation_key),
        inactivity_timeout_sec: args.inactivity_timeout_sec,
        obfuscation_size: args.obfuscation_size,
        pad_size: args.pad_size,
    });

    // For each inbound socket, spawn handler, client -> server
    for (index, sock) in ctx.inbound_sockets.iter().enumerate() {
        let sock_clone = sock.clone();
        let ctx_clone = ctx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                let (n, remote_addr) = match sock_clone.recv_from(&mut buf).await {
                    Ok(x) => x,
                    Err(_) => continue,
                };

                if n < ctx_clone.pad_size {
                    // invalid packet ignore
                    continue;
                }

                let n = unpad_package(&buf, n, ctx_clone.pad_size);

                let mut data = buf[..n].to_vec();
                obfuscation_in_place(&mut data, &ctx_clone.key_bytes, ctx_clone.obfuscation_size);

                if let Some(conn_info) =
                    ensure_upstream_socket(index, remote_addr, ctx_clone.clone()).await
                {
                    let conn_info = conn_info.read().await;
                    // Send data upstream
                    let _ = conn_info.upstream_socket.send(&data).await;
                } else {
                    println!("No upstream socket for {:?}", remote_addr);
                }
            }
        });
    }

    // Cleanup task
    let cleanup_ctx = ctx.clone();
    tokio::spawn(async move {
        let check_interval = Duration::from_secs((cleanup_ctx.inactivity_timeout_sec / 2) as u64);
        loop {
            sleep(check_interval).await;

            let mut map_guard = cleanup_ctx.connection_map.lock().await;

            let mut remove_ips = Vec::new();
            let now = now_ts();
            for (ip, info) in map_guard.iter() {
                let info = info.read().await;
                println!(
                    "Checking IP: {:?} {} {}",
                    ip,
                    now,
                    info.last_client_packet_ts.load(Ordering::Relaxed)
                );
                if now - info.last_client_packet_ts.load(Ordering::Relaxed)
                    > cleanup_ctx.inactivity_timeout_sec
                    || info.upstream_read_loop_exit
                {
                    remove_ips.push(*ip);
                }
            }

            for ip in remove_ips {
                if let Some(_) = map_guard.remove(&ip) {
                    //
                }
            }
        }
    });

    println!("Server proxy started. Press Ctrl+C to stop.");
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

/// Called whenever a packet arrives from `remote_addr`.
async fn ensure_upstream_socket(
    index: usize,
    remote_addr: SocketAddr,
    ctx: Arc<ServerContext>,
) -> Option<Arc<RwLock<ConnectionInfo>>> {
    let remote_ip = remote_addr.ip();

    {
        let map_guard = ctx.connection_map.lock().await;
        if let Some(info) = map_guard.get(&remote_ip) {
            let info_guard = info.read().await;
            info_guard
                .last_client_packet_ts
                .store(now_ts(), Ordering::Relaxed);
            let recent_addresses = info_guard.recent_addresses.read().await;
            if Some(remote_addr) != recent_addresses[index] {
                drop(recent_addresses);
                let mut recent_addresses = info_guard.recent_addresses.write().await;
                recent_addresses[index] = Some(remote_addr);
            }
            return Some(info.clone());
        }
    }
    let upstream_addresses = parse_addresses(&ctx.upstream).await;

    if upstream_addresses.is_empty() {
        println!("No valid upstream addresses for server.");
        return None;
    }

    // No existing record -> create upstream
    let upstream_socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .expect("Failed to bind upstream socket"),
    );

    let _ = upstream_socket.connect(upstream_addresses[0]).await;

    let conn_info = Arc::new(RwLock::new(ConnectionInfo {
        upstream_socket: upstream_socket.clone(),
        recent_addresses: RwLock::new(vec![None; ctx.inbound_sockets.len()]),
        last_client_packet_ts: Arc::new(AtomicU32::new(now_ts())),
        upstream_read_loop_exit: false,
    }));

    {
        conn_info.write().await.recent_addresses.write().await[index] = Some(remote_addr);
    }

    {
        let mut map_guard = ctx.connection_map.lock().await;
        map_guard.insert(remote_ip, conn_info.clone());
    }

    // Spawn read loop from upstream -> inbound
    tokio::spawn(upstream_read_loop(
        Arc::downgrade(&upstream_socket),
        conn_info.clone(),
        ctx.clone(),
    ));

    Some(conn_info)
}

/// upstream_read_loop: receives data from upstream -> server upstream socket,
/// applies XOR, picks a address from the `recent_addresses`, sends back to client.
async fn upstream_read_loop(
    socket: Weak<UdpSocket>,
    conn_info: Arc<RwLock<ConnectionInfo>>,
    ctx: Arc<ServerContext>,
) {
    let mut buf = vec![0u8; 65535];
    let mut seq: usize = 0;
    loop {
        let socket = match socket.upgrade() {
            Some(socket) => socket,
            None => break, // socket has been dropped
        };
        let (n, _src) = match socket.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(_) => break, // socket closed
        };

        let info_guard = conn_info.read().await;
        let addresses = &info_guard.recent_addresses.read().await;
        let indexes: Vec<usize> = addresses
            .iter()
            .enumerate()
            .filter_map(|(index, addr)| if addr.is_some() { Some(index) } else { None })
            .collect();
        if indexes.is_empty() {
            continue; // technically should not happen
        }
        let index = indexes[seq % indexes.len()];
        seq = (seq + 1) % usize::MAX;
        let target_addr = addresses[index].unwrap();
        let inbound_sock = &ctx.inbound_sockets[index];
        obfuscation_in_place(&mut buf[..n], &ctx.key_bytes, ctx.obfuscation_size);
        let n = pad_package(&mut buf, n, &ctx.key_bytes, ctx.pad_size);
        let _ = inbound_sock.send_to(&buf[..n], target_addr).await;
    }
}
