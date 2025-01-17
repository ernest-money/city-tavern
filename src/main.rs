#![allow(dead_code, unused_variables)]
mod routed_message;

use bitcoin::{key::rand::Fill, secp256k1::PublicKey};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
// use dlc_messages::message_handler::MessageHandler as DlcMessageHandler;
use lightning::{
    ln::peer_handler::{
        CustomMessageHandler, ErroringMessageHandler, IgnoringMessageHandler, MessageHandler,
        PeerManager,
    },
    sign::{KeysManager, NodeSigner},
    util::logger::Logger,
};
use lightning_net_tokio::{connect_outbound, setup_inbound, SocketDescriptor};
use routed_message::{ProxyMessageHandler, RoutedMessage};
use tokio::net::TcpListener;

#[derive(Clone, Debug, Copy)]
struct Log;

impl Logger for Log {
    fn log(&self, record: lightning::util::logger::Record) {
        println!("{}", record.args);
    }
}

type ErnestProxy = PeerManager<
    SocketDescriptor,
    Arc<ErroringMessageHandler>,
    Arc<IgnoringMessageHandler>,
    Arc<IgnoringMessageHandler>,
    Arc<Log>,
    Arc<ProxyMessageHandler>,
    Arc<KeysManager>,
>;

struct CityTavern {
    peer_manager: Arc<ErnestProxy>,
    pub message_handler: Arc<ProxyMessageHandler>,
    connected_peers: Arc<Mutex<HashMap<PublicKey, bool>>>,
    key_manager: Arc<KeysManager>,
    stop_signal: tokio::sync::watch::Receiver<bool>,
    listening_port: u16,
    node_id: PublicKey,
}

impl CityTavern {
    fn new(stop_signal: tokio::sync::watch::Receiver<bool>, listening_port: u16) -> Self {
        let logger = Log {};

        let mut rand_bytes = [0u8; 32];
        rand_bytes
            .try_fill(&mut bitcoin::key::rand::thread_rng())
            .unwrap();

        let dlc_message_handler = Arc::new(ProxyMessageHandler::default());
        let message_handler = MessageHandler {
            chan_handler: Arc::new(ErroringMessageHandler::new()),
            route_handler: Arc::new(IgnoringMessageHandler {}),
            onion_message_handler: Arc::new(IgnoringMessageHandler {}),
            custom_message_handler: dlc_message_handler.clone(),
        };

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let node_signer = Arc::new(KeysManager::new(
            &rand_bytes,
            current_time.into(),
            current_time,
        ));
        let node_id = node_signer
            .get_node_id(lightning::sign::Recipient::Node)
            .unwrap();
        let peer_manager: Arc<ErnestProxy> = Arc::new(PeerManager::new(
            message_handler,
            current_time,
            &rand_bytes,
            Arc::new(logger),
            node_signer.clone(),
        ));

        let connected_peers = Arc::new(Mutex::new(HashMap::new()));

        Self {
            peer_manager,
            message_handler: dlc_message_handler,
            key_manager: node_signer,
            stop_signal,
            listening_port,
            connected_peers,
            node_id,
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.key_manager
            .get_node_id(lightning::sign::Recipient::Node)
            .unwrap()
    }

    async fn listen(&self) -> anyhow::Result<()> {
        println!("Listening on port {}", self.listening_port);
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.listening_port))
            .await
            .expect("Coldn't get port.");

        let mut stop_receiver = self.stop_signal.clone();
        let peer_manager = self.peer_manager.clone();
        loop {
            tokio::select! {
                _ = stop_receiver.changed() => {
                    if *stop_receiver.borrow() {
                        println!("Stopping listener.");
                        break;
                    }
                }
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((tcp_stream, _socket)) => {
                            let peer_mgr = Arc::clone(&peer_manager);
                            tokio::spawn(async move {
                                println!("Received connection.");
                                setup_inbound(peer_mgr, tcp_stream.into_std().unwrap()).await;
                            });
                        }
                        Err(e) => {
                            eprintln!("Error accepting connection: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn process_messages(&self) {
        let mut event_ticker = tokio::time::interval(Duration::from_secs(1));
        let peer_manager = self.peer_manager.clone();
        let message_handler = self.message_handler.clone();
        let connected_peers = self.connected_peers.clone();
        let mut stop_signal = self.stop_signal.clone();
        let midnight_rider = self.node_id.clone();
        loop {
            tokio::select! {
                _ = stop_signal.changed() => {
                    if *stop_signal.borrow() {
                        println!("Stopping listener.");
                        break;
                    }
                }
                _ = event_ticker.tick() => {
                    peer_manager.process_events();
                    event_handler(midnight_rider, message_handler.clone(), connected_peers.clone()).await;

                }
            }
        }
    }

    async fn connect_to_peer(&self, peer_id: PublicKey, addr: &str) -> anyhow::Result<()> {
        let peer_manager = self.peer_manager.clone();
        connect_outbound(peer_manager, peer_id, addr.parse().unwrap()).await;
        Ok(())
    }

    fn send_message(&self, msg: RoutedMessage) -> anyhow::Result<()> {
        self.message_handler
            .send_message(msg)
            .expect("failed to send message");
        Ok(())
    }
}

async fn event_handler(
    midnight_rider: PublicKey,
    message_handler: Arc<ProxyMessageHandler>,
    connected_peers: Arc<Mutex<HashMap<PublicKey, bool>>>,
) {
    let events = message_handler.get_and_clear_pending_msg();

    for event in events {
        if event.1.from != event.0 {
            println!("Message is not from who it should be: {:?}", event);
        }

        if event.1.to == midnight_rider {
            println!("Received an offer from: {:?}", event.1.from);
        }

        if connected_peers.lock().await.contains_key(&event.0) {
            println!("Sending message to: {:?}", event.1.to);
            message_handler.send_message(event.1).unwrap()
        }
    }
}

#[tokio::main]
async fn main() {
    let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
    let tavern = Arc::new(CityTavern::new(stop_receiver, 1778));

    let _ = TcpListener::bind(format!("0.0.0.0:{}", 1778))
        .await
        .expect("Coldn't get port.");

    println!(
        "node id: {}",
        tavern
            .key_manager
            .get_node_id(lightning::sign::Recipient::Node)
            .unwrap()
            .to_string()
    );

    let tavern_clone = tavern.clone();
    tokio::spawn(async move {
        tavern_clone.process_messages().await;
    });
    // tokio::spawn(async move {
    tavern.listen().await.unwrap();
    // });

    // tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    // stop_signal.send(true).unwrap();
}

#[cfg(test)]
mod tests {
    use dlc_messages::AcceptDlc;
    use routed_message::RoutedMessage;

    use super::*;

    #[tokio::test]
    async fn test_listen() {
        let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
        let tavern = Arc::new(CityTavern::new(stop_receiver.clone(), 1776));
        let tavern_two = Arc::new(CityTavern::new(stop_receiver, 1778));

        let tavern_one_clone = tavern.clone();
        tokio::spawn(async move {
            tavern_one_clone.listen().await.unwrap();
        });

        let tavern_two_clone = tavern_two.clone();
        tokio::spawn(async move {
            tavern_two_clone.listen().await.unwrap();
        });

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        tavern
            .connect_to_peer(tavern_two.public_key(), "127.0.0.1:1778")
            .await
            .unwrap();

        let accept_msg = include_str!("./test_inputs/accept_msg.json");
        let accept_msg: AcceptDlc = serde_json::from_str(&accept_msg).unwrap();
        let routed_msg = RoutedMessage {
            to: tavern_two.public_key(),
            from: tavern.public_key(),
            message: dlc_messages::Message::Accept(accept_msg),
        };

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        tavern.message_handler.send_message(routed_msg).unwrap();

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        println!("processing events");
        tavern.peer_manager.process_events();
        tavern_two.peer_manager.process_events();
        let msgs = tavern_two.message_handler.get_and_clear_received_messages();
        println!("msgs: {:?}", msgs);
        stop_signal.send(true).unwrap();
    }
}
