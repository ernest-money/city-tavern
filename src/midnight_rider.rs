use std::{
    collections::{HashMap, VecDeque},
    fmt::Display,
    sync::{Arc, Mutex},
};

use bitcoin::{io::Cursor, secp256k1::PublicKey};
use dlc_messages::segmentation;
use lightning::{
    io::Read,
    ln::{
        features::{InitFeatures, NodeFeatures},
        msgs::{DecodeError, Init, LightningError},
        peer_handler::CustomMessageHandler,
        wire::{CustomMessageReader, Type},
    },
    util::ser::{Readable, Writeable, MAX_BUF_SIZE},
};

use crate::routed_message::{ProxyMessage, RoutedMessage};

pub struct MidnightRider {
    events: Mutex<VecDeque<(PublicKey, ProxyMessage)>>,
    received_messages: Mutex<Vec<(PublicKey, RoutedMessage)>>,
    connected_peers: Arc<Mutex<HashMap<PublicKey, bool>>>,
    segment_readers: Arc<Mutex<HashMap<PublicKey, segmentation::segment_reader::SegmentReader>>>,
    proxy_node_id: PublicKey,
    my_node_id: PublicKey,
}

impl MidnightRider {
    pub fn new(proxy_node_id: PublicKey, my_node_id: PublicKey) -> Self {
        Self {
            events: Mutex::new(VecDeque::new()),
            received_messages: Mutex::new(Vec::new()),
            connected_peers: Arc::new(Mutex::new(HashMap::new())),
            segment_readers: Arc::new(Mutex::new(HashMap::new())),
            proxy_node_id,
            my_node_id,
        }
    }

    fn is_proxy_node(&self) -> bool {
        self.my_node_id == self.proxy_node_id
    }

    pub fn send_message(&self, msg: RoutedMessage, node_id: PublicKey) {
        tracing::info!("sending message to {:?}", node_id);
        if msg.serialized_length() > MAX_BUF_SIZE {
            let (seg_start, seg_chunks) = segmentation::get_segments(msg.encode(), msg.type_id());
            let mut msg_events = self.events.lock().unwrap();
            msg_events.push_back((node_id, ProxyMessage::SegmentStart(seg_start)));
            for chunk in seg_chunks {
                msg_events.push_back((node_id, ProxyMessage::SegmentChunk(chunk)));
            }
        } else {
            self.events
                .lock()
                .unwrap()
                .push_back((node_id, ProxyMessage::RoutedMessage(msg)));
        }
    }

    pub fn handle_routed_message(
        &self,
        message: RoutedMessage,
        sender_node_id: &PublicKey,
    ) -> Result<(), LightningError> {
        // handle the proxy stuff
        let msg_clone = message.clone();

        // handle receiving an offer to the proxy
        if self.is_proxy_node() && message.to == self.proxy_node_id {
            tracing::info!("Received an offer to the proxy.");
            self.received_messages
                .lock()
                .unwrap()
                .push((message.from, msg_clone));
            // assert that it is an actual offer
            return Ok(());
        }

        // handle receiving a proxied message
        if sender_node_id == &self.proxy_node_id && message.to == self.my_node_id {
            tracing::info!("Received a proxied message: {:?}", sender_node_id);
            self.received_messages
                .lock()
                .unwrap()
                .push((message.from, msg_clone));
            return Ok(());
        }

        if &message.from != sender_node_id {
            tracing::info!("Message is not from who it should be. Invalid message:");
        }

        if self
            .connected_peers
            .lock()
            .unwrap()
            .contains_key(&message.to)
        {
            tracing::info!("Received a proxy connection: {:?}", message.to);
            let route_msg = RoutedMessage {
                msg_type: message.msg_type,
                to: message.to,
                from: message.from,
                message: message.message,
            };
            self.send_message(route_msg.clone(), route_msg.to)
        } else {
            tracing::info!("Not connected to the peer: {}", message.to.to_string())
        }

        Ok(())
    }

    /// Returns the messages received by the message handler and empty the
    /// receiving buffer.
    pub fn get_and_clear_received_messages(&self) -> Vec<(PublicKey, RoutedMessage)> {
        let mut ret = Vec::new();
        std::mem::swap(&mut *self.received_messages.lock().unwrap(), &mut ret);
        ret
    }
}

impl CustomMessageReader for MidnightRider {
    type CustomMessage = ProxyMessage;
    fn read<R: Read>(
        &self,
        msg_type: u16,
        buffer: &mut R,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        let decoded = match msg_type {
            segmentation::SEGMENT_START_TYPE => ProxyMessage::SegmentStart(Readable::read(buffer)?),
            segmentation::SEGMENT_CHUNK_TYPE => ProxyMessage::SegmentChunk(Readable::read(buffer)?),
            _ => {
                let message: RoutedMessage = Readable::read(buffer)?;
                ProxyMessage::RoutedMessage(message)
            }
        };

        Ok(Some(decoded))
    }
}

impl CustomMessageHandler for MidnightRider {
    fn handle_custom_message(
        &self,
        msg: Self::CustomMessage,
        sender_node_id: &PublicKey,
    ) -> Result<(), LightningError> {
        let mut segment_readers = self.segment_readers.lock().unwrap();
        let segment_reader = segment_readers.entry(*sender_node_id).or_default();

        if segment_reader.expecting_chunk() {
            match msg {
                ProxyMessage::SegmentChunk(s) => {
                    if let Some(msg) = segment_reader
                        .process_segment_chunk(s)
                        .map_err(|e| to_ln_error(e, "Error processing segment chunk"))?
                    {
                        let mut buf = Cursor::new(msg);
                        let message_type = <u16 as Readable>::read(&mut buf).map_err(|e| {
                            to_ln_error(e, "Could not reconstruct message from segments")
                        })?;
                        if let ProxyMessage::RoutedMessage(message) = self
                            .read(message_type, &mut buf)
                            .map_err(|e| {
                                to_ln_error(e, "Could not reconstruct message from segments")
                            })?
                            .expect("to have a message")
                        {
                            self.handle_routed_message(message, sender_node_id)?;
                        } else {
                            return Err(to_ln_error(
                                "Unexpected message type",
                                &message_type.to_string(),
                            ));
                        }
                    }
                    return Ok(());
                }
                _ => {
                    // We were expecting a segment chunk but received something
                    // else, we reset the state.
                    segment_reader.reset();
                }
            }
        }

        match msg {
            ProxyMessage::RoutedMessage(message) => {
                self.handle_routed_message(message, sender_node_id)?
            }
            ProxyMessage::SegmentStart(s) => segment_reader
                .process_segment_start(s)
                .map_err(|e| to_ln_error(e, "Error processing segment start"))?,
            ProxyMessage::SegmentChunk(_) => {
                return Err(LightningError {
                    err: "Received a SegmentChunk while not expecting one.".to_string(),
                    action: lightning::ln::msgs::ErrorAction::DisconnectPeer { msg: None },
                });
            }
        };
        Ok(())
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        self.events.lock().unwrap().drain(..).collect()
    }

    fn peer_disconnected(&self, their_node_id: &PublicKey) {
        self.connected_peers.lock().unwrap().remove(their_node_id);
        tracing::info!("Peer disconnected: {:?}", their_node_id.to_string());
    }

    fn peer_connected(
        &self,
        their_node_id: &PublicKey,
        _msg: &Init,
        _inbound: bool,
    ) -> Result<(), ()> {
        tracing::info!("Peer connected: {:?}", their_node_id.to_string());
        self.connected_peers
            .lock()
            .unwrap()
            .insert(their_node_id.clone(), true);
        Ok(())
    }

    fn provided_node_features(&self) -> NodeFeatures {
        NodeFeatures::empty()
    }

    fn provided_init_features(&self, _their_node_id: &PublicKey) -> InitFeatures {
        InitFeatures::empty()
    }
}

#[inline]
fn to_ln_error<T: Display>(e: T, msg: &str) -> LightningError {
    LightningError {
        err: format!("{} :{}", msg, e),
        action: lightning::ln::msgs::ErrorAction::DisconnectPeer { msg: None },
    }
}
