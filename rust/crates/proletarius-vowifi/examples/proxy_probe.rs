use std::env;
use std::net::SocketAddr;

use proletarius_vowifi::Socks5UdpRelay;

#[tokio::main]
async fn main() {
    let address = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:6153".to_owned());
    let proxy: SocketAddr = address.parse().unwrap_or_else(|error| {
        eprintln!("invalid proxy address {address:?}: {error}");
        std::process::exit(2);
    });
    match Socks5UdpRelay::associate(proxy).await {
        Ok(_) => println!("proxy={proxy} socks5_udp_associate=ready direct_udp=false"),
        Err(error) => {
            eprintln!("proxy={proxy} socks5_udp_associate=unavailable error={error}");
            std::process::exit(1);
        }
    }
}
