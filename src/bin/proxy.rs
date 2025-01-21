use bitcoin::bip32::Xpriv;
use bitcoin::key::rand;
use bitcoin::Network;
use city_tavern::CityTavern;
use rand::Fill;
use std::env::current_dir;
use std::sync::Arc;
use std::time::Duration;
use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};
use tokio::signal;
use tracing::level_filters::LevelFilter;

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::fmt()
        .with_line_number(true)
        .with_max_level(LevelFilter::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let seed_bytes = xprv_from_path(current_dir().unwrap(), Network::Regtest).unwrap();

    let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
    let tavern = Arc::new(
        CityTavern::new(Some(&seed_bytes.private_key.secret_bytes()), 1776, None).unwrap(),
    );

    tracing::info!("City Tavern node id: {}", tavern.public_key());

    let tavern_clone = tavern.clone();
    let stop_signal_clone = stop_receiver.clone();
    tokio::spawn(async move {
        let _ = tavern_clone.listen(stop_signal_clone).await;
    });

    let mut messages_ticker = tokio::time::interval(Duration::from_secs(1));
    let mut send_messages_ticker = tokio::time::interval(Duration::from_secs(2));
    let message_clone = tavern.clone();
    let mut msg_stop_signal_clone = stop_receiver.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = msg_stop_signal_clone.changed() => {
                    if *msg_stop_signal_clone.borrow() {
                        tracing::info!("Stopping listener.");
                        break;
                    }
                }
                _ = messages_ticker.tick() => {
                    let messages = message_clone.midnight_rider.get_and_clear_received_messages();
                    if messages.len() > 0 {
                        tracing::info!("Received messages: {:?}", messages);
                    }
                }
                _ = send_messages_ticker.tick() => {
                    if message_clone.midnight_rider.has_pending_messages() {
                        tracing::info!("Sending messages that need to be sent.");
                        message_clone.process_msgs_to_send();
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("Ctrl+C received, stopping...");
            stop_signal.send(true).unwrap();
        }
    }
}

/// Helper function that reads `[bitcoin::bip32::Xpriv]` bytes from a file.
/// If the file does not exist then it will create a file `seed.ddk` in the specified path.
pub fn xprv_from_path(path: PathBuf, network: Network) -> anyhow::Result<Xpriv> {
    let seed_path = path.join("city-tavern.seed");
    let seed = if Path::new(&seed_path).exists() {
        let seed = std::fs::read(&seed_path)?;
        let mut key = [0; 32];
        key.copy_from_slice(&seed);
        Xpriv::new_master(network, &seed)?
    } else {
        let mut file = File::create(&seed_path)?;
        let mut entropy = [0u8; 32];
        entropy.try_fill(&mut rand::thread_rng())?;
        // let _mnemonic = Mnemonic::from_entropy(&entropy)?;
        let xprv = Xpriv::new_master(network, &entropy)?;
        file.write_all(&entropy)?;
        xprv
    };

    Ok(seed)
}
