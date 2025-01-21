# City Tavern Project

## Overview

The City Tavern project is a decentralized communication framework designed for handling messages in a peer-to-peer network. It utilizes the Lightning Network protocol and is built using Rust, providing a robust and efficient way to manage and route messages between nodes. The project is particularly focused on handling DLC (Discreet Log Contracts) messages, enabling secure and reliable transactions.

## Capabilities

- **Peer-to-Peer Communication**: Establishes connections between nodes and facilitates message exchange.
- **Message Routing**: Routes messages through a proxy node, allowing for efficient communication even in complex network topologies.
- **DLC Support**: Specifically designed to handle DLC messages, including offers, accepts, and rejections.
- **Segmentation**: Supports message segmentation for larger messages, ensuring that they can be transmitted without exceeding buffer limits.
- **Logging and Monitoring**: Integrates with the `tracing` library for logging, providing insights into the system's operations.

## Components

- **CityTavern**: The main struct that manages peer connections and message processing.
- **MidnightRider**: Handles the routing of messages and manages the state of connected peers.
- **RoutedMessage**: Represents a message that can be routed through the network, encapsulating the message type, sender, receiver, and the actual message content.
- **Transport**: An interface for sending and receiving messages, allowing for different implementations depending on the underlying transport mechanism.

## Getting Started

### Prerequisites

- Rust (version 1.56 or higher)
- Cargo (Rust package manager)

### Installation

1. Clone the repository:

   ```bash
   git clone https://github.com/yourusername/city-tavern.git
   cd city-tavern
   ```

2. Build the project:
   ```bash
   cargo build
   ```

### Usage

1. **Running the Node**: You can start a node by running the `main` function in `src/bin/proxy.rs`. This will initialize the City Tavern node and start listening for incoming connections.

   ```bash
   cargo run --bin proxy
   ```

2. **Connecting Peers**: Use the `connect_to_peer` method in the `CityTavern` struct to connect to other nodes in the network. You can specify the peer's public key and address.

3. **Sending Messages**: To send a message, use the `send_message` method, providing the recipient's public key and the message content.

4. **Handling Messages**: Implement the `CustomMessageHandler` trait in your node to define how incoming messages should be processed.
