use city_tavern::CityTavern;
use std::sync::Arc;
use tracing::level_filters::LevelFilter;

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::fmt()
        .with_line_number(true)
        .with_max_level(LevelFilter::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
    let tavern = Arc::new(CityTavern::new(1776, None).unwrap());

    tracing::info!("City Tavern node id: {}", tavern.public_key());

    let tavern_clone = tavern.clone();
    let stop_signal_clone = stop_receiver.clone();

    tokio::spawn(async move {
        let _ = tavern_clone.listen(stop_signal_clone).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    stop_signal.send(true).unwrap();
}
