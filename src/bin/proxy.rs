use midnight_rider::CityTavern;
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
    let tavern = Arc::new(CityTavern::new(stop_receiver, 1778, None).unwrap());

    let _ = TcpListener::bind(format!("0.0.0.0:{}", 1778))
        .await
        .expect("Coldn't get port.");

    println!("City Tavern node id: {}", tavern.public_key());

    let tavern_clone = tavern.clone();
    tokio::spawn(async move {
        tavern_clone.process_messages().await;
    });
    tokio::spawn(async move {
        tavern.listen().await.unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    stop_signal.send(true).unwrap();
}
