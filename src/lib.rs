#![allow(dead_code, unused_variables)]
mod midnight_rider;
mod routed_message;
mod transport;

use anyhow::anyhow;
use bitcoin::{key::rand::Fill, secp256k1::PublicKey};
use ddk::{DlcDevKitDlcManager, Oracle, Storage};
use lightning::{
    ln::{
        msgs::{ErrorAction, LightningError},
        peer_handler::{
            ErroringMessageHandler, IgnoringMessageHandler, MessageHandler, PeerDetails,
            PeerManager,
        },
    },
    sign::{KeysManager, NodeSigner},
    util::logger::{Level, Logger},
};
use lightning_net_tokio::{connect_outbound, setup_inbound, SocketDescriptor};
use midnight_rider::MidnightRider;
use routed_message::{RoutedMessage, ACCEPT_TYPE, OFFER_TYPE, SIGN_TYPE};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle, time::interval};

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
        seed_bytes: Option<&[u8; 32]>,
        listening_port: u16,
        // If None, the node will be a proxy node.
        proxy_node_id: Option<PublicKey>,
    ) -> anyhow::Result<Self> {
        let logger = Log {};

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        let seed_bytes = if let Some(seed_bytes) = seed_bytes {
            seed_bytes.to_owned()
        } else {
            let mut rand_bytes = [0u8; 32];
            rand_bytes.try_fill(&mut bitcoin::key::rand::thread_rng())?;
            rand_bytes
        };

        let node_signer = Arc::new(KeysManager::new(
            &seed_bytes,
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

        let mut ephemeral_bytes = [0u8; 32];
        ephemeral_bytes.try_fill(&mut bitcoin::key::rand::thread_rng())?;

        let peer_manager: Arc<CityTavenPeerManager> = Arc::new(PeerManager::new(
            message_handler,
            current_time,
            &ephemeral_bytes,
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

    pub fn list_peers(&self) -> Vec<PeerDetails> {
        self.peer_manager.list_peers()
    }

    pub fn process_msgs_to_send(&self) {
        self.peer_manager.process_events();
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
            tracing::info!(port = listening_port, "Started City Tavern listener.");
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

    pub fn process_messages<S: Storage, O: Oracle>(
        &self,
        stop_signal: watch::Receiver<bool>,
        manager: Arc<DlcDevKitDlcManager<S, O>>,
    ) -> JoinHandle<Result<(), anyhow::Error>> {
        let mut message_stop = stop_signal.clone();
        let message_manager = Arc::clone(&manager);
        let peer_manager = Arc::clone(&self.peer_manager);
        let midnight_rider = Arc::clone(&self.midnight_rider);
        let public_key = self.public_key();
        tokio::spawn(async move {
            let mut message_interval = interval(Duration::from_secs(5));
            let mut process_interval = interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    _ = message_stop.changed() => {
                        if *message_stop.borrow() {
                            tracing::warn!("Stop signal for lightning message processor.");
                            break;
                        }
                    },
                    _ = message_interval.tick() => {
                        let messages = midnight_rider.get_and_clear_received_messages();
                        for (counter_party, routed_message) in messages {
                            tracing::info!(
                                counter_party = counter_party.to_string(),
                                routed_message = routed_message.from.to_string(),
                                "Processing DLC message"
                            );
                            match message_manager.on_dlc_message(&routed_message.message, routed_message.from).await {
                                Ok(Some(message)) => {
                                    // maybe check if the proxy is connected
                                    tracing::info!(
                                        to = routed_message.from.to_string(),
                                        from = public_key.to_string(),
                                        message = ?message,
                                        "Midnight rider sending response message.",
                                    );
                                    let routed_msg = RoutedMessage {
                                        to: routed_message.from,
                                        from: public_key,
                                        msg_type: message_to_message_type(&message).unwrap(),
                                        message: message,
                                    };

                                    midnight_rider.send_message(routed_msg);
                                }
                                Ok(None) => (),
                                Err(e) => {
                                    tracing::error!(
                                        error=e.to_string(),
                                        counterparty=counter_party.to_string(),
                                        message=?routed_message.message,
                                        "Could not process dlc message."
                                    );
                                }
                            }
                        }
                    }
                    _ = process_interval.tick() => {
                        if midnight_rider.has_pending_messages() {
                            tracing::info!("Sending messages that need to be processed.");
                            peer_manager.process_events();
                        }
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

pub(crate) fn message_to_message_type(
    message: &dlc_messages::Message,
) -> Result<u16, LightningError> {
    match message {
        dlc_messages::Message::Offer(_) => Ok(OFFER_TYPE),
        dlc_messages::Message::Accept(_) => Ok(ACCEPT_TYPE),
        dlc_messages::Message::Sign(_) => Ok(SIGN_TYPE),
        _ => Err(LightningError {
            err: "Invalid message type for message.".to_string(),
            action: ErrorAction::IgnoreAndLog(Level::Error),
        }),
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
        let proxy = Arc::new(CityTavern::new(None, 1781, None).unwrap());
        let alice = Arc::new(CityTavern::new(None, 1776, Some(proxy.public_key())).unwrap());
        let bob = Arc::new(CityTavern::new(None, 1778, Some(proxy.public_key())).unwrap());

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
        alice.midnight_rider.send_message(routed_msg);
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
        let proxy = Arc::new(CityTavern::new(None, 1781, None).unwrap());
        let alice = Arc::new(CityTavern::new(None, 1776, Some(proxy.public_key())).unwrap());
        let bob = Arc::new(CityTavern::new(None, 1778, Some(proxy.public_key())).unwrap());

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

        alice.midnight_rider.send_message(routed_msg);

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
