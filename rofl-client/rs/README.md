# rofl-client/rs/README.md
Rust client for the ROFL appd over UNIX domain sockets and plain HTTP.

- Default socket: `/run/rofl-appd.sock`
- HTTP constructor: `RoflClient::with_url("http://localhost:8549")`
- Endpoints used:
  - `GET /rofl/v1/app/id`
  - `POST /rofl/v1/keys/generate`
  - `POST /rofl/v1/tx/sign-submit`
  - `GET /rofl/v1/metadata`
  - `POST /rofl/v1/metadata`
  - `POST /rofl/v1/query`

Quickstart:

```rust
use oasis_rofl_client::{KeyKind, RoflClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = match std::env::var("ROFL_CLIENT_URL") {
        Ok(url) => RoflClient::with_url(&url)?,
        Err(_) => RoflClient::new()?,
    };

    println!("app id: {}", client.get_app_id().await?);
    println!(
        "key: {}",
        client.generate_key("example", KeyKind::Ed25519).await?
    );
    Ok(())
}
```

Notes:
- `RoflClient::new()` and `RoflClient::with_socket_path()` keep using UNIX sockets.
- `RoflClient::with_url()` accepts `http://...` URLs and custom socket paths; `https://...` is rejected explicitly.
- Methods are `async` and internally offload blocking transport I/O via `tokio::task::spawn_blocking`.
