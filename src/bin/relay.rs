//! Raspberry Pi / Linux relay for team sync. TLS is terminated by a reverse proxy.
use std::net::TcpListener;
use summoner_watch::relay::{serve, Config};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let access_key = std::env::var("SUMMONER_RELAY_KEY").unwrap_or_default();
    if !access_key.is_empty() && (access_key.len() != 32 || !access_key.bytes().all(|c| c.is_ascii_hexdigit())) {
        return Err("SUMMONER_RELAY_KEY must be empty (public relay) or a random 32-character hex key".into());
    }
    let ip_header = std::env::var("SUMMONER_RELAY_IP_HEADER").ok().filter(|h| !h.trim().is_empty());
    let bind = std::env::var("SUMMONER_RELAY_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = TcpListener::bind(&bind)?;
    println!(
        "Summoner Watch relay listening on {bind} ({})",
        if access_key.is_empty() { "public" } else { "private, relay key required" }
    );
    if ip_header.is_none() {
        println!("SUMMONER_RELAY_IP_HEADER is not set: behind a reverse proxy all clients share one IP and its limits");
    }
    serve(listener, Config { access_key, ip_header });
    Ok(())
}
