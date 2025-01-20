use std::{
    collections::{HashMap, VecDeque},
    fmt::{Debug, Display},
    sync::{Arc, Mutex},
};

use bitcoin::{io::Cursor, secp256k1::PublicKey};
use dlc_messages::segmentation;
use dlc_messages::{message_handler::read_dlc_message, Message, WireMessage};
use lightning::{
    io::Read,
    ln::{
        features::{InitFeatures, NodeFeatures},
        msgs::{DecodeError, Init, LightningError},
        peer_handler::CustomMessageHandler,
        wire::{CustomMessageReader, Type},
    },
    util::ser::{Readable, Writeable, Writer, MAX_BUF_SIZE},
};

/// The type prefix for a routed message.
const ROUTED_MESSAGE_TYPE_ID: u16 = 1776;

/// The type prefix for an [`OfferDlc`] message.
pub const OFFER_TYPE: u16 = 42778;

/// The type prefix for an [`AcceptDlc`] message.
pub const ACCEPT_TYPE: u16 = 42780;

/// The type prefix for an [`SignDlc`] message.
pub const SIGN_TYPE: u16 = 42782;

/// The type prefix for a [`Reject`] message.
pub const REJECT: u16 = 43024;

#[derive(Debug, Clone)]
pub struct RoutedMessage {
    pub msg_type: u16,
    pub to: PublicKey,
    pub from: PublicKey,
    pub message: Message,
}

impl Type for RoutedMessage {
    fn type_id(&self) -> u16 {
        ROUTED_MESSAGE_TYPE_ID
    }
}

impl Writeable for RoutedMessage {
    fn write<W: Writer>(&self, writer: &mut W) -> lightning::io::Result<()> {
        writer.write_all(&self.msg_type.to_le_bytes())?;
        self.to.write(writer)?;
        self.from.write(writer)?;
        self.message.write(writer)?;
        Ok(())
    }
}

impl Readable for RoutedMessage {
    fn read<R: Read>(reader: &mut R) -> Result<Self, DecodeError> {
        let mut type_buf = [0u8; 2];
        reader.read_exact(&mut type_buf)?;
        let msg_type = u16::from_le_bytes(type_buf);

        // First read pubkeys
        let mut buf = [0u8; 33]; // Compressed pubkey is 33 bytes
        reader.read_exact(&mut buf)?;
        let to = PublicKey::from_slice(&buf).map_err(|_| DecodeError::InvalidValue)?;

        reader.read_exact(&mut buf)?;
        let from = PublicKey::from_slice(&buf).map_err(|_| DecodeError::InvalidValue)?;

        // Use the existing DLC message reading function
        let wire_msg = read_dlc_message(msg_type, reader)?.ok_or(DecodeError::InvalidValue)?;

        // Extract inner message from WireMessage
        // do segment dance
        let message = match wire_msg {
            WireMessage::Message(msg) => msg,
            _ => {
                return Err(DecodeError::InvalidValue);
            }
        };

        Ok(RoutedMessage {
            msg_type,
            to,
            from,
            message,
        })
    }
}

pub enum ProxyMessage {
    RoutedMessage(RoutedMessage),
    SegmentStart(segmentation::SegmentStart),
    SegmentChunk(segmentation::SegmentChunk),
}

macro_rules! impl_type_writeable_for_enum {
    ($type_name: ident, {$($variant_name: ident),*}) => {
       impl Type for $type_name {
           fn type_id(&self) -> u16 {
               match self {
                   $($type_name::$variant_name(v) => v.type_id(),)*
               }
           }
       }

       impl Writeable for $type_name {
            fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ::lightning::io::Error> {
                match self {
                   $($type_name::$variant_name(v) => v.write(writer),)*
                }
            }
       }
    };
}

impl Debug for ProxyMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::RoutedMessage(_) => "RoutedMessage",
            Self::SegmentStart(_) => "SegmentStart",
            Self::SegmentChunk(_) => "SegmentChunk",
        };
        f.write_str(name)
    }
}

impl_type_writeable_for_enum!(ProxyMessage, { RoutedMessage, SegmentStart, SegmentChunk });

pub struct ProxyMessageHandler {
    events: Mutex<VecDeque<(PublicKey, ProxyMessage)>>,
    received_messages: Mutex<Vec<(PublicKey, RoutedMessage)>>,
    connected_peers: Arc<Mutex<HashMap<PublicKey, bool>>>,
    segment_readers: Arc<Mutex<HashMap<PublicKey, segmentation::segment_reader::SegmentReader>>>,
    proxy_node_id: PublicKey,
    my_node_id: PublicKey,
}

impl ProxyMessageHandler {
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

    pub fn send_message(&self, msg: RoutedMessage, node_id: PublicKey) -> anyhow::Result<()> {
        println!("sending message to {:?}", node_id);
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
        Ok(())
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
            println!("Received an offer to the proxy.");
            self.received_messages
                .lock()
                .unwrap()
                .push((message.from, msg_clone));
            // assert that it is an actual offer
            return Ok(());
        }

        // handle receiving a proxied message
        if sender_node_id == &self.proxy_node_id && message.to == self.my_node_id {
            println!("Received a proxied message: {:?}", sender_node_id);
            self.received_messages
                .lock()
                .unwrap()
                .push((message.from, msg_clone));
            return Ok(());
        }

        if &message.from != sender_node_id {
            println!("Message is not from who it should be. Invalid message:");
        }

        if self
            .connected_peers
            .lock()
            .unwrap()
            .contains_key(&message.to)
        {
            println!("Received a proxy connection: {:?}", message.to);
            let route_msg = RoutedMessage {
                msg_type: message.msg_type,
                to: message.to,
                from: message.from,
                message: message.message,
            };
            self.send_message(route_msg.clone(), route_msg.to).unwrap()
        } else {
            println!("Not connected to the peer: {}", message.to.to_string())
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

impl CustomMessageReader for ProxyMessageHandler {
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

impl CustomMessageHandler for ProxyMessageHandler {
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
        println!("Peer disconnected: {:?}", their_node_id.to_string());
    }

    fn peer_connected(
        &self,
        their_node_id: &PublicKey,
        msg: &Init,
        inbound: bool,
    ) -> Result<(), ()> {
        println!("Peer connected: {:?}", their_node_id.to_string());
        self.connected_peers
            .lock()
            .unwrap()
            .insert(their_node_id.clone(), true);
        Ok(())
    }

    fn provided_node_features(&self) -> NodeFeatures {
        NodeFeatures::empty()
    }

    fn provided_init_features(&self, their_node_id: &PublicKey) -> InitFeatures {
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
