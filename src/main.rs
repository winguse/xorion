mod client;
mod helpers;
mod server;
mod singular;
mod tests;

use clap::Parser;
use helpers::*;
use tests::profiling;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    profiling(true).await;

    let args = Args::parse();
    let obfuscation_key = parse_obfuscation_key(&args.obfuscation_key);

    if args.server && args.client {
        println!("Cannot run as both server and client at the same time.");
        std::process::exit(1);
    } else if args.singular {
        singular::singular_main(&args, obfuscation_key).await;
    } else if args.server {
        server::server_main(&args, obfuscation_key).await?;
    } else if args.client {
        client::client_main(&args, obfuscation_key).await?;
    } else {
        println!("Must specify --server or --client.");
        std::process::exit(1);
    }

    Ok(())
}
