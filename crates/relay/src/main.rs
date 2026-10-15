//! WinterMiles group-ride relay.
//!
//!     wintermiles-relay [address]      (default 0.0.0.0:47800)

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "0.0.0.0:47800".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("WinterMiles relay listening on ws://{addr}");
    wintermiles_relay::serve(listener).await
}
