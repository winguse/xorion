use clap::{ArgAction, Parser};
use rand::{rngs::StdRng, seq::IteratorRandom, Rng, SeedableRng};
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::lookup_host;

#[derive(Parser, Debug)]
#[command(
    name = "xorion",
    author = "Yingyu Cheng <1443504+winguse@users.noreply.github.com>",
    version = "0.0.1",
    about = "A multi-path UDP proxy applying XOR obfuscation."
)]
pub struct Args {
    /// Run in server mode
    #[arg(long, action = ArgAction::SetTrue)]
    pub server: bool,

    /// Run in client mode
    #[arg(long, action = ArgAction::SetTrue)]
    pub client: bool,

    /// Run in singular mode (only one upstream socket address)
    /// This will have better security but better performance, but cannot by pass ISP QoS.
    #[arg(long, action = ArgAction::SetTrue)]
    pub singular: bool,

    /// For server: comma-separated ports. For client: single port.
    #[arg(long)]
    pub bind: String,

    /// Upstream addresses
    #[arg(long)]
    pub upstream: String,

    /// XOR key (hex or decimal). Defaults to 0.
    #[arg(long, default_value = "0")]
    pub obfuscation_key: String,

    /// Inactivity timeout in seconds (for both server & client).
    #[arg(long, default_value = "60")]
    pub inactivity_timeout_sec: u32,

    /// Maximum obfuscation size. Defaults to 140.
    #[arg(long, default_value = "140")]
    pub obfuscation_size: usize,

    /// min size to transmit, or pad to this size. Defaults to 140.
    #[arg(long, default_value = "140")]
    pub pad_size: usize,
}

pub const RND_BUF_SIZE: usize = 4099;
const SEED_SIZE: usize = 4;

pub fn pad_package(
    buf: &mut [u8],
    size: usize,
    key_bytes: &[u8; RND_BUF_SIZE],
    pad_size: usize,
) -> usize {
    if size >= pad_size {
        return size;
    }

    for i in size..pad_size - 1 {
        buf[i] ^= key_bytes[buf[i] as usize];
    }
    buf[pad_size - 1] = size as u8;
    pad_size
}

pub fn unpad_package(buf: &[u8], size: usize, pad_size: usize) -> usize {
    let original_size = buf[pad_size - 1] as usize;
    if size != pad_size || original_size >= pad_size {
        // we only pad to fixed size, for others ignore
        return size;
    }
    original_size
}

pub fn obfuscation_in_place(
    buf: &mut [u8],
    key_bytes: &[u8; RND_BUF_SIZE],
    obfuscation_size: usize,
) {
    let len = buf.len();

    if len < SEED_SIZE {
        for i in 0..len {
            buf[i] ^= key_bytes[i];
        }
        return;
    }

    let mut ki = u32::from_le_bytes(
        buf[len - SEED_SIZE..len]
            .try_into()
            .expect("Slice with incorrect length"),
    ) as usize
        % RND_BUF_SIZE;

    let end = obfuscation_size.min(len - SEED_SIZE);
    for i in 0..end {
        buf[i] ^= key_bytes[ki];
        ki += 1;
        if ki >= RND_BUF_SIZE {
            ki = 0;
        }
    }
}

pub fn generate_random_buf(input: u64) -> [u8; RND_BUF_SIZE] {
    let mut rng = StdRng::seed_from_u64(input);
    let mut buffer = [0u8; RND_BUF_SIZE];
    rng.fill(&mut buffer[0..4096]);
    rng.fill(&mut buffer[4096..RND_BUF_SIZE]);
    buffer
}

pub fn parse_obfuscation_key(s: &str) -> u64 {
    if let Some(stripped) = s.strip_prefix("0x") {
        u64::from_str_radix(stripped, 16).unwrap_or(0)
    } else {
        s.parse().unwrap_or(0)
    }
}

pub fn now_ts() -> u32 {
    return SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs() as u32;
}

pub async fn parse_addresses(input: &str) -> Vec<SocketAddr> {
    let parts: Vec<&str> = input.split(':').collect();

    // If the input doesn't have exactly 2 parts, return an empty vector
    if parts.len() != 2 {
        return vec![];
    }

    let host = parts[0];
    let ports: Vec<&str> = parts[1].split(',').collect();

    // Perform DNS lookup for the hostname
    match lookup_host(format!("{}:{}", host, ports[0])).await {
        Ok(socket_addresses) => {
            // Choose a random IP address from the resolved addresses
            if let Some(socket_address) = socket_addresses.choose(&mut rand::thread_rng()) {
                // Map the ports to SocketAddr with the chosen IP address
                ports
                    .into_iter()
                    .filter_map(|port_str| {
                        port_str.parse::<u16>().ok().map(|port| {
                            let mut addr = socket_address.clone();
                            addr.set_port(port);
                            addr
                        })
                    })
                    .collect()
            } else {
                println!("Failed to lookup host: {}", host);
                // Return an empty vector if no addresses were found
                vec![]
            }
        }
        Err(_) => {
            // Return an empty vector if the DNS lookup fails
            println!("Failed to lookup host: {}", host);
            vec![]
        }
    }
}
