#![allow(dead_code, unused_variables)]
mod midnight_rider;
mod routed_message;
mod transport;

use anyhow::anyhow;
use bitcoin::{key::rand::Fill, secp256k1::PublicKey};
use lightning::{
    ln::peer_handler::{
        ErroringMessageHandler, IgnoringMessageHandler, MessageHandler, PeerManager,
    },
    sign::{KeysManager, NodeSigner},
    util::logger::{Level, Logger},
};
use lightning_net_tokio::{connect_outbound, setup_inbound, SocketDescriptor};
use midnight_rider::MidnightRider;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

#[derive(Clone, Debug, Copy)]
struct Log;

impl Logger for Log {
    fn log(&self, record: lightning::util::logger::Record) {
        match record.level {
            Level::Info => tracing::info!("{}", record.args),
            Level::Debug => tracing::debug!("{}", record.args),
            _ => (),
        }
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
    pub midnight_rider: Arc<MidnightRider>,
    listening_port: u16,
    node_id: PublicKey,
}

impl CityTavern {
    pub fn new(
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

        let midnight_rider = Arc::new(MidnightRider::new(
            proxy_node_id.unwrap_or(my_node_id),
            my_node_id,
        ));

        let message_handler = MessageHandler {
            chan_handler: Arc::new(ErroringMessageHandler::new()),
            route_handler: Arc::new(IgnoringMessageHandler {}),
            onion_message_handler: Arc::new(IgnoringMessageHandler {}),
            custom_message_handler: midnight_rider.clone(),
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
            midnight_rider,
            listening_port,
            node_id: my_node_id,
        })
    }

    pub fn public_key(&self) -> PublicKey {
        self.node_id
    }

    pub fn listen(
        &self,
        stop_signal: watch::Receiver<bool>,
    ) -> JoinHandle<Result<(), anyhow::Error>> {
        let mut stop_receiver = stop_signal.clone();
        let peer_manager = self.peer_manager.clone();
        let listening_port = self.listening_port.clone();
        tokio::spawn(async move {
            let listener = TcpListener::bind(format!("0.0.0.0:{}", listening_port)).await?;
            tracing::info!("Listening on port {}", listening_port);
            loop {
                tokio::select! {
                    _ = stop_receiver.changed() => {
                        if *stop_receiver.borrow() {
                            tracing::info!("Stopping listener.");
                            break;
                        }
                    }
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((tcp_stream, _socket)) => {
                                let peer_mgr = Arc::clone(&peer_manager);
                                tokio::spawn(async move {
                                    tracing::info!("Received connection.");
                                    setup_inbound(peer_mgr, tcp_stream.into_std().unwrap()).await;
                                });
                            }
                            Err(e) => {
                                tracing::error!("Error accepting connection: {}", e);
                            }
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        })
    }

    pub fn process_messages(
        &self,
        stop_signal: watch::Receiver<bool>,
    ) -> JoinHandle<Result<(), anyhow::Error>> {
        let mut event_ticker = tokio::time::interval(Duration::from_secs(1));
        let peer_manager = self.peer_manager.clone();
        let mut stop_signal = stop_signal.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop_signal.changed() => {
                    if *stop_signal.borrow() {
                        tracing::info!("Stopping listener.");
                        break;
                    }
                }
                _ = event_ticker.tick() => {
                        peer_manager.process_events();
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        })
    }

    pub async fn connect_to_peer(&self, peer_id: PublicKey, addr: &str) {
        let _ = connect_outbound(self.peer_manager.clone(), peer_id, addr.parse().unwrap()).await;
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
        let proxy = Arc::new(CityTavern::new(1781, None).unwrap());
        let alice = Arc::new(CityTavern::new(1776, Some(proxy.public_key())).unwrap());
        let bob = Arc::new(CityTavern::new(1778, Some(proxy.public_key())).unwrap());

        let alice_clone = alice.clone();
        let stop_signal_clone = stop_receiver.clone();
        tokio::spawn(async move {
            alice_clone.listen(stop_signal_clone);
        });

        let bob_clone = bob.clone();
        let stop_signal_clone = stop_receiver.clone();
        tokio::spawn(async move {
            bob_clone.listen(stop_signal_clone);
        });

        let proxy_clone = proxy.clone();
        let stop_signal_clone = stop_receiver.clone();
        tokio::spawn(async move {
            proxy_clone.listen(stop_signal_clone);
        });

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        alice
            .connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await;

        bob.connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await;

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
            .midnight_rider
            .send_message(routed_msg, proxy.public_key());
        alice.peer_manager.process_events();
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        let msgs = proxy.midnight_rider.get_and_clear_received_messages();

        // no messages on proxy
        assert!(msgs.len() == 0);
        proxy.peer_manager.process_events();

        let bob_msgs = bob.midnight_rider.get_and_clear_received_messages();

        assert!(bob_msgs.len() > 0);

        stop_signal.send(true).unwrap();
    }

    #[tokio::test]
    async fn offer() {
        let (stop_signal, stop_receiver) = tokio::sync::watch::channel(false);
        let proxy = Arc::new(CityTavern::new(1781, None).unwrap());
        let alice = Arc::new(CityTavern::new(1776, Some(proxy.public_key())).unwrap());
        let bob = Arc::new(CityTavern::new(1778, Some(proxy.public_key())).unwrap());

        let alice_clone = alice.clone();
        let stop_signal_clone = stop_receiver.clone();
        tokio::spawn(async move {
            alice_clone.listen(stop_signal_clone);
        });

        let bob_clone = bob.clone();
        let stop_signal_clone = stop_receiver.clone();
        tokio::spawn(async move {
            bob_clone.listen(stop_signal_clone);
        });

        let proxy_clone = proxy.clone();
        let stop_signal_clone = stop_receiver.clone();
        tokio::spawn(async move {
            proxy_clone.listen(stop_signal_clone);
        });

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        alice
            .connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await;

        bob.connect_to_peer(proxy.public_key(), "127.0.0.1:1781")
            .await;

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
            .midnight_rider
            .send_message(routed_msg, proxy.public_key());

        alice.peer_manager.process_events();
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        let msgs = proxy.midnight_rider.get_and_clear_received_messages();

        // the proxy receives the offer and does not send message
        assert!(msgs.len() > 0);
        proxy.peer_manager.process_events();

        let bob_msgs = bob.midnight_rider.get_and_clear_received_messages();

        assert!(bob_msgs.len() == 0);

        stop_signal.send(true).unwrap();
    }
}
