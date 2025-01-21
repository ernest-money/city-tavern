use std::sync::Arc;

use crate::message_to_message_type;
use crate::{routed_message::RoutedMessage, CityTavern};
use bitcoin::secp256k1::PublicKey;
use ddk::DlcDevKitDlcManager;
use ddk::{Oracle, Storage, Transport};
use tokio::sync::watch;

#[async_trait::async_trait]
impl Transport for CityTavern {
    fn name(&self) -> String {
        "city-tavern".to_string()
    }

    fn public_key(&self) -> PublicKey {
        self.public_key()
    }

    async fn start<S: Storage, O: Oracle>(
        &self,
        mut stop_signal: watch::Receiver<bool>,
        manager: Arc<DlcDevKitDlcManager<S, O>>,
    ) -> Result<(), anyhow::Error> {
        let listen_handle = self.listen(stop_signal.clone());

        let process_handle = self.process_messages(stop_signal.clone(), manager);

        // Wait for either task to complete or stop signal
        tokio::select! {
            _ = stop_signal.changed() => Ok(()),
            res = listen_handle => res?,
            res = process_handle => res?,
        }
    }

    async fn send_message(&self, peer_id: PublicKey, msg: dlc_messages::Message) {
        let routed_msg = RoutedMessage {
            to: peer_id,
            from: self.public_key(),
            msg_type: message_to_message_type(&msg).unwrap(),
            message: msg,
        };
        self.midnight_rider.send_message(routed_msg);
    }

    async fn connect_outbound(&self, peer_id: PublicKey, addr: &str) {
        let _ = self.connect_to_peer(peer_id, addr).await;
    }
}
