use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use bitcoin::secp256k1::PublicKey;
use dlc_messages::{message_handler::read_dlc_message, Message, WireMessage};
use lightning::{
    io::Read,
    ln::{
        features::{InitFeatures, NodeFeatures},
        msgs::{DecodeError, Init, LightningError},
        peer_handler::CustomMessageHandler,
        wire::{CustomMessageReader, Type},
    },
    util::ser::{Readable, Writeable, Writer},
};

/// The type prefix for an [`OfferDlc`] message.
pub const OFFER_TYPE: u16 = 42778;

/// The type prefix for an [`AcceptDlc`] message.
pub const ACCEPT_TYPE: u16 = 42780;

/// The type prefix for an [`SignDlc`] message.
pub const SIGN_TYPE: u16 = 42782;

/// The type prefix for a [`Reject`] message.
pub const REJECT: u16 = 43024;

const ROUTED_MESSAGE_TYPE_ID: u16 = 1776;

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

#[derive(Default)]
pub struct ProxyMessageHandler {
    events: Mutex<Vec<(PublicKey, RoutedMessage)>>,
    received_messages: Mutex<Vec<(PublicKey, RoutedMessage)>>,
    connected_peers: Arc<Mutex<HashMap<PublicKey, bool>>>,
}

impl ProxyMessageHandler {
    pub fn send_message(&self, msg: RoutedMessage, to: PublicKey) -> anyhow::Result<()> {
        println!("sending message to {:?}", msg.to);
        self.events.lock().unwrap().push((to, msg));
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
    type CustomMessage = RoutedMessage;
    fn read<R: Read>(
        &self,
        msg_type: u16,
        buffer: &mut R,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        match msg_type {
            ROUTED_MESSAGE_TYPE_ID => {
                let message: RoutedMessage = Readable::read(buffer)?;
                Ok(Some(message))
            }
            _ => Ok(None),
        }
    }
}

impl CustomMessageHandler for ProxyMessageHandler {
    fn handle_custom_message(
        &self,
        msg: Self::CustomMessage,
        sender_node_id: &PublicKey,
    ) -> Result<(), LightningError> {
        println!(
            "Received routed message from {:?}: {:?}",
            sender_node_id, msg
        );

        let msg_clone = msg.clone();

        if &msg.from != sender_node_id {
            println!("Message is not from who it should be: {:?}", msg.message);
        }

        if msg.to == msg.from {
            println!("Received an offer from: {:?}", msg.from);
        }

        if self.connected_peers.lock().unwrap().contains_key(&msg.to) {
            println!("Received a proxy connection: {:?}", msg.to);
            let route_msg = RoutedMessage {
                msg_type: msg.msg_type,
                to: msg.to,
                from: msg.from,
                message: msg.message,
            };
            self.send_message(route_msg.clone(), route_msg.to).unwrap()
        } else {
            println!("Not connected to the peer: {}", msg.to.to_string())
        }

        self.received_messages
            .lock()
            .unwrap()
            .push((msg.from, msg_clone));
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
