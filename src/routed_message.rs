use std::fmt::Debug;

use bitcoin::secp256k1::PublicKey;
use dlc_messages::segmentation;
use dlc_messages::{message_handler::read_dlc_message, Message, WireMessage};
use lightning::{
    io::Read,
    ln::{msgs::DecodeError, wire::Type},
    util::ser::{Readable, Writeable, Writer},
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
