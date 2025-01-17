use std::sync::Mutex;

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

const ROUTED_MESSAGE_TYPE_ID: u16 = 1776;

#[derive(Debug, Clone)]
pub struct RoutedMessage {
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
        self.to.write(writer)?;
        self.from.write(writer)?;
        self.message.write(writer)?;
        Ok(())
    }
}

impl Readable for RoutedMessage {
    fn read<R: Read>(reader: &mut R) -> Result<Self, DecodeError> {
        // First read pubkeys
        let mut buf = [0u8; 33]; // Compressed pubkey is 33 bytes
        reader.read_exact(&mut buf)?;
        let to = PublicKey::from_slice(&buf).map_err(|_| DecodeError::InvalidValue)?;

        reader.read_exact(&mut buf)?;
        let from = PublicKey::from_slice(&buf).map_err(|_| DecodeError::InvalidValue)?;

        // Now read the message type to determine which inner message to decode
        let inner_type = <u16 as Readable>::read(reader)?;

        // Use the existing DLC message reading function
        let wire_msg = read_dlc_message(inner_type, reader)?.ok_or(DecodeError::InvalidValue)?;

        // Extract inner message from WireMessage
        let message = match wire_msg {
            WireMessage::Message(msg) => msg,
            _ => return Err(DecodeError::InvalidValue),
        };

        Ok(RoutedMessage { to, from, message })
    }
}

#[derive(Default)]
pub struct ProxyMessageHandler {
    events: Mutex<Vec<(PublicKey, RoutedMessage)>>,
    received_messages: Mutex<Vec<(PublicKey, RoutedMessage)>>,
}

impl ProxyMessageHandler {
    pub fn send_message(&self, msg: RoutedMessage) -> anyhow::Result<()> {
        println!("sending message to {:?}", msg.to);
        self.events.lock().unwrap().push((msg.to, msg));
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

        self.received_messages.lock().unwrap().push((msg.from, msg));
        Ok(())
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        self.events.lock().unwrap().drain(..).collect()
    }

    fn peer_disconnected(&self, their_node_id: &PublicKey) {
        println!("Peer disconnected: {:?}", their_node_id);
    }

    fn peer_connected(
        &self,
        their_node_id: &PublicKey,
        msg: &Init,
        inbound: bool,
    ) -> Result<(), ()> {
        println!("Peer connected: {:?}", their_node_id);
        Ok(())
    }

    fn provided_node_features(&self) -> NodeFeatures {
        NodeFeatures::empty()
    }

    fn provided_init_features(&self, their_node_id: &PublicKey) -> InitFeatures {
        InitFeatures::empty()
    }
}
