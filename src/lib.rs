#![allow(dead_code, unused_variables)]
mod midnight_rider;
mod routed_message;

use anyhow::anyhow;
use bitcoin::{key::rand::Fill, secp256k1::PublicKey};
use lightning::{
    ln::peer_handler::{
        ErroringMessageHandler, IgnoringMessageHandler, MessageHandler, PeerManager,
    },
    sign::{KeysManager, NodeSigner},
    util::logger::Logger,
};
use lightning_net_tokio::{connect_outbound, setup_inbound, SocketDescriptor};
use midnight_rider::MidnightRider;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;

#[derive(Clone, Debug, Copy)]
struct Log;

impl Logger for Log {
    fn log(&self, record: lightning::util::logger::Record) {
        // println!("{}", record.args);
    }
}

type CityTavenPeerManager = PeerManager<
    SocketDescriptor,
    Arc<ErroringMessageHandler>,
    Arc<IgnoringMessageHandler>,
    Arc<IgnoringMessageHandler>,
    Arc<Log>,
    Arc<MidnightRider>,
    Arc<KeysManager>,
>;

pub struct CityTavern {
    peer_manager: Arc<CityTavenPeerManager>,
    pub message_handler: Arc<MidnightRider>,
    key_manager: Arc<KeysManager>,
    stop_signal: tokio::sync::watch::Receiver<bool>,
    listening_port: u16,
    node_id: PublicKey,
}

impl CityTavern {
    pub fn new(
        stop_signal: tokio::sync::watch::Receiver<bool>,
        listening_port: u16,
        // If None, the node will be a proxy node.
        proxy_node_id: Option<PublicKey>,
    ) -> anyhow::Result<Self> {
        let logger = Log {};

        let mut rand_bytes = [0u8; 32];
        rand_bytes.try_fill(&mut bitcoin::key::rand::thread_rng())?;

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let node_signer = Arc::new(KeysManager::new(
            &rand_bytes,
            current_time.into(),
            current_time,
        ));

        let my_node_id = node_signer
            .get_node_id(lightning::sign::Recipient::Node)
            .map_err(|_| anyhow!("Couldn't get node id"))?;

        let dlc_message_handler = Arc::new(MidnightRider::new(
            proxy_node_id.unwrap_or(my_node_id),
            my_node_id,
        ));

        let message_handler = MessageHandler {
            chan_handler: Arc::new(ErroringMessageHandler::new()),
            route_handler: Arc::new(IgnoringMessageHandler {}),
            onion_message_handler: Arc::new(IgnoringMessageHandler {}),
            custom_message_handler: dlc_message_handler.clone(),
        };

        let peer_manager: Arc<CityTavenPeerManager> = Arc::new(PeerManager::new(
            message_handler,
            current_time,
            &rand_bytes,
            Arc::new(logger),
            node_signer.clone(),
        ));

        Ok(Self {
            peer_manager,
            message_handler: dlc_message_handler,
            key_manager: node_signer,
            stop_signal,
            listening_port,
            node_id: my_node_id,
        })
    }

    pub fn public_key(&self) -> PublicKey {
        self.node_id
    }

    pub async fn listen(&self) -> anyhow::Result<()> {
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

    pub async fn process_messages(&self) {
        let mut event_ticker = tokio::time::interval(Duration::from_secs(1));
        let peer_manager = self.peer_manager.clone();
        let message_handler = self.message_handler.clone();
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
                }
            }
        }
    }

    async fn connect_to_peer(&self, peer_id: PublicKey, addr: &str) -> anyhow::Result<()> {
        connect_outbound(self.peer_manager.clone(), peer_id, addr.parse().unwrap()).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use dlc_messages::{AcceptDlc, OfferDlc};
    use routed_message::{RoutedMessage, ACCEPT_TYPE, OFFER_TYPE};

    use super::*;

    #[tokio::test]
    async fn accept() {
        let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
        let proxy = Arc::new(CityTavern::new(stop_receiver.clone(), 1781, None).unwrap());
        let alice = Arc::new(
            CityTavern::new(stop_receiver.clone(), 1776, Some(proxy.public_key())).unwrap(),
        );
        let bob = Arc::new(CityTavern::new(stop_receiver, 1778, Some(proxy.public_key())).unwrap());

        let alice_clone = alice.clone();
        tokio::spawn(async move {
            alice_clone.listen().await.unwrap();
        });

        let bob_clone = bob.clone();
        tokio::spawn(async move {
            bob_clone.listen().await.unwrap();
        });

        let proxy_clone = proxy.clone();
        tokio::spawn(async move {
            proxy_clone.listen().await.unwrap();
        });

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        alice
            .connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await
            .unwrap();

        bob.connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await
            .unwrap();

        let accept_msg = include_str!("./test_inputs/accept_msg.json");
        let accept_msg: AcceptDlc = serde_json::from_str(&accept_msg).unwrap();
        let routed_msg = RoutedMessage {
            msg_type: ACCEPT_TYPE,
            to: bob.public_key(),
            from: alice.public_key(),
            message: dlc_messages::Message::Accept(accept_msg),
        };

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        alice
            .message_handler
            .send_message(routed_msg, proxy.public_key())
            .unwrap();
        alice.peer_manager.process_events();
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        let msgs = proxy.message_handler.get_and_clear_received_messages();

        // no messages on proxy
        assert!(msgs.len() == 0);
        proxy.peer_manager.process_events();

        let bob_msgs = bob.message_handler.get_and_clear_received_messages();

        assert!(bob_msgs.len() > 0);

        stop_signal.send(true).unwrap();
    }

    #[tokio::test]
    async fn offer() {
        let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
        let proxy = Arc::new(CityTavern::new(stop_receiver.clone(), 1781, None).unwrap());
        let alice = Arc::new(
            CityTavern::new(stop_receiver.clone(), 1776, Some(proxy.public_key())).unwrap(),
        );
        let bob = Arc::new(CityTavern::new(stop_receiver, 1778, Some(proxy.public_key())).unwrap());

        let alice_clone = alice.clone();
        tokio::spawn(async move {
            alice_clone.listen().await.unwrap();
        });

        let bob_clone = bob.clone();
        tokio::spawn(async move {
            bob_clone.listen().await.unwrap();
        });

        let proxy_clone = proxy.clone();
        tokio::spawn(async move {
            proxy_clone.listen().await.unwrap();
        });

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        alice
            .connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await
            .unwrap();

        bob.connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await
            .unwrap();

        let offer_msg = include_str!("./test_inputs/offer_msg.json");
        let offer_msg: OfferDlc = serde_json::from_str(&offer_msg).unwrap();
        let routed_msg = RoutedMessage {
            msg_type: OFFER_TYPE,
            to: proxy.public_key(),
            from: alice.public_key(),
            message: dlc_messages::Message::Offer(offer_msg),
        };

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        alice
            .message_handler
            .send_message(routed_msg, proxy.public_key())
            .unwrap();

        alice.peer_manager.process_events();
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        let msgs = proxy.message_handler.get_and_clear_received_messages();

        // the proxy receives the offer and does not send message
        assert!(msgs.len() > 0);
        proxy.peer_manager.process_events();

        let bob_msgs = bob.message_handler.get_and_clear_received_messages();

        assert!(bob_msgs.len() == 0);

        stop_signal.send(true).unwrap();
    }
}
