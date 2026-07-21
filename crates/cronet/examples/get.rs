use cronet::{Engine, Request};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://example.com".to_owned());
    let engine = Engine::builder().user_agent("cronet-rs/example").build()?;
    let request = Request::builder(url)?.build()?;
    let mut response = engine.send(request).await?;
    let mut body = Vec::new();
    response.body.read_to_end(&mut body).await?;
    let finished = response.body.finished().await?;
    println!("HTTP {}", response.status());
    println!("{}", String::from_utf8_lossy(&body));
    println!("finished: {:?}", finished.reason);
    engine.shutdown().await?;
    Ok(())
}
