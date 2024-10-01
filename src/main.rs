use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use bitcoin::key::rand::Fill;
use lightning::{
    ln::peer_handler::{
        ErroringMessageHandler, IgnoringMessageHandler, MessageHandler, PeerManager,
    },
    sign::{KeysManager, NodeSigner},
    util::logger::Logger,
};
use lightning_net_tokio::{setup_inbound, SocketDescriptor};
use tokio::net::TcpListener;

#[derive(Clone, Debug, Copy)]
struct Log;

impl Logger for Log {
    fn log(&self, record: lightning::util::logger::Record) {
        print!("{:?}", record);
    }
}

type ErnestProxy = PeerManager<
    SocketDescriptor,
    Arc<ErroringMessageHandler>,
    Arc<IgnoringMessageHandler>,
    Arc<IgnoringMessageHandler>,
    Arc<Log>,
    Arc<IgnoringMessageHandler>,
    Arc<KeysManager>,
>;

#[tokio::main]
async fn main() {
    let logger = Log {};

    let mut rand_bytes = [0u8; 32];
    rand_bytes
        .try_fill(&mut bitcoin::key::rand::thread_rng())
        .unwrap();

    let message_handler = MessageHandler {
        chan_handler: Arc::new(ErroringMessageHandler::new()),
        route_handler: Arc::new(IgnoringMessageHandler {}),
        onion_message_handler: Arc::new(IgnoringMessageHandler {}),
        custom_message_handler: Arc::new(IgnoringMessageHandler {}),
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
    let peer_manager: Arc<ErnestProxy> = Arc::new(PeerManager::new(
        message_handler,
        current_time,
        &rand_bytes,
        Arc::new(logger),
        node_signer.clone(),
    ));

    let listener = TcpListener::bind(format!("0.0.0.0:{}", 1778))
        .await
        .expect("Coldn't get port.");

    println!(
        "node id: {}",
        node_signer
            .get_node_id(lightning::sign::Recipient::Node)
            .unwrap()
            .to_string()
    );

    loop {
        let peer_mgr = peer_manager.clone();
        let (tcp_stream, _socket) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            println!("Received connection.");
            setup_inbound(peer_mgr.clone(), tcp_stream.into_std().unwrap()).await;
        });
    }
}
