//! Demo signaling server T3: bind 0.0.0.0:8420 + advertise mDNS.
//!
//! Run dari root repo:
//!     cargo run -p wdt-core --example signaling_server_demo [port]
//!
//! Mencetak token pairing + string `ip:port:token` untuk fallback manual/QR.
//! Berhenti dengan Ctrl-C. Untuk Test B, di terminal lain jalankan:
//!     dns-sd -B _wdt._tcp
//!     dns-sd -L "<instance>" _wdt._tcp local.

use std::net::{IpAddr, SocketAddr};

use wdt_core::signaling::discovery::MdnsAdvert;
use wdt_core::signaling::protocol::DEFAULT_PORT;
use wdt_core::signaling::server;

fn local_ip_simple() -> String {
    // Tentukan IP lokal via koneksi UDP keluar (tanpa traffic nyata).
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").expect("udp bind");
    // 8.8.8.8:80 tidak pernah dihubungi (connect UDP = lokal saja).
    let _ = sock.connect("8.8.8.8:80");
    sock.local_addr()
        .map(|a| match a.ip() {
            IpAddr::V4(v) => v.to_string(),
            IpAddr::V6(v) => v.to_string(),
        })
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("wdt_core=debug,info")
        .try_init()
        .ok();
    let port: u16 = std::env::args()
        .nth(1)
        .map(|s| s.parse().expect("port harus angka"))
        .unwrap_or(DEFAULT_PORT);

    // Token tetap opsional untuk pengujian (agar receiver/TV bisa reconnect
    // tanpa scan ulang): WDT_TOKEN=859200 <binary> [port]
    let token = std::env::var("WDT_TOKEN").ok().filter(|t| !t.is_empty());
    let server = server::spawn_with_token(SocketAddr::from(([0, 0, 0, 0], port)), token).await?;
    let ip = local_ip_simple();
    let hostname = hostname_simple();
    let instance = format!("WDT {}", short_host(&hostname));

    let advert = MdnsAdvert::advertise(&instance, &format!("{hostname}.local."), &ip, port)?;
    println!("signaling server: {}", server.local_addr);
    println!("mDNS instance:    {}", advert.fullname());
    println!("pairing token:    {}", server.token);
    println!("pairing string:   {}:{}:{}", ip, port, server.token);
    println!("endpoint WS:      ws://{ip}:{port}/ws");
    println!("(Ctrl-C untuk berhenti)");

    tokio::signal::ctrl_c().await?;
    println!("shutdown...");
    advert.stop();
    server.stop().await;
    Ok(())
}

fn hostname_simple() -> String {
    // Tanpa dependency hostname: baca via gethostname libc? Sederhana:
    // pakai variabel env umum, fallback literal.
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "sender".to_string())
}

fn short_host(h: &str) -> String {
    // Potong domain + batasi 11 char agar "WDT {x}" <= 15 char.
    let short = h.split('.').next().unwrap_or(h);
    short.chars().take(11).collect()
}
