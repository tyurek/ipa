use crate::helpers::{
    CommandEnvelope, HelperIdentity, NetworkEventData, SubscriptionType, TransportCommand,
};
use crate::protocol::QueryId;
use crate::task::JoinHandle;
use futures::StreamExt;
use futures_util::stream::SelectAll;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use ::tokio::sync::{mpsc, oneshot};
use futures_util::future::poll_immediate;
use tokio_stream::wrappers::ReceiverStream;
#[cfg(all(feature = "shuttle", test))]
use shuttle::future as tokio;
use tracing::Instrument;

#[derive(Debug)]
enum SwitchCommand {
    Subscribe(SubscribeRequest),
    Halt
}

struct SubscribeRequest {
    subscription: SubscriptionType,
    link: mpsc::Sender<CommandEnvelope>,
    ack_tx: oneshot::Sender<()>,
}

impl Debug for SubscribeRequest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Subscribe[{:?}]", self.subscription)
    }
}

impl SubscribeRequest {
    pub fn new(
        subscription: SubscriptionType,
        link: mpsc::Sender<CommandEnvelope>,
    ) -> (Self, oneshot::Receiver<()>) {
        let (ack_tx, ack_rx) = oneshot::channel();
        (
            Self {
                subscription,
                link,
                ack_tx,
            },
            ack_rx,
        )
    }

    pub fn acknowledge(self) {
        self.ack_tx.send(()).unwrap();
    }

    pub fn subscription(&self) -> SubscriptionType {
        self.subscription
    }

    pub fn sender(&self) -> mpsc::Sender<CommandEnvelope> {
        self.link.clone()
    }
}

/// State of the demultiplexer
#[derive(Debug)]
enum State {
    /// Getting ready to start receiving commands. In this state, it is possible to add new
    /// peer connections
    Idle(
        mpsc::Receiver<SwitchCommand>,
        HashMap<HelperIdentity, mpsc::Receiver<TransportCommand>>,
    ),
    /// Interim state where demultiplexer is about to start actively listening for incoming commands
    Preparing,
    /// Actively listening. It is no longer possible to change the demultiplexer's state.
    Listening(JoinHandle<()>),
}

/// Takes care of forwarding commands received from multiple links (one link per peer)
/// to the subscribers
pub(super) struct Switch {
    state: State,
    tx: mpsc::Sender<SwitchCommand>,
    id: HelperIdentity
}

impl Debug for Switch {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "mux[{:?}]", self.state)
    }
}

impl Switch {
    pub fn new(id: HelperIdentity) -> Self {
        let (tx, rx) = mpsc::channel(1);

        Self {
            state: State::Idle(rx, HashMap::default()),
            tx,
            id,
        }
    }

    pub fn new_peer(&mut self, peer_id: HelperIdentity, peer_rx: mpsc::Receiver<TransportCommand>) {
        let State::Idle(_, peers) = &mut self.state else {
            panic!("Not in Idle state");
        };

        assert!(peers.insert(peer_id, peer_rx).is_none());
    }

    /// Starts listening to the incoming messages in a separate task. Can only be called once
    /// and only when it is in the `Idle` state.
    pub fn listen(&mut self) {
        let State::Idle(mut rx, peers) = std::mem::replace(&mut self.state, State::Preparing) else {
            panic!("Not in Idle state");
        };

        let mut peer_links = SelectAll::new();
        for (addr, link) in peers {
            peer_links.push(ReceiverStream::new(link).map(move |command| (addr.clone(), command)));
        }
        let handle = tokio::spawn(async move {
            let mut query_router = QueryCommandRouter::default();
            loop {
                ::tokio::select! {
                    Some(command) = rx.recv() => {
                        match command {
                            SwitchCommand::Subscribe(subscribe_command) => {
                                match subscribe_command.subscription() {
                                    SubscriptionType::Query(query_id) => {
                                        tracing::trace!("Subscribed to receive commands for query {query_id:?}");
                                        query_router.subscribe(query_id, subscribe_command.sender());
                                        subscribe_command.acknowledge();
                                    },
                                    SubscriptionType::Administration => {
                                        unimplemented!()
                                    }
                                }
                            }
                            SwitchCommand::Halt => {
                                tracing::trace!("Switch is terminated");
                                break;
                            }
                        }
                    }
                    Some((origin, command)) = peer_links.next() => {
                        tracing::trace!("received new command {command:?} from {origin:?}");
                        match command {
                            TransportCommand::NetworkEvent(data) => query_router.route(origin, data).await
                        }
                        tracing::trace!("command processed");
                    }
                    else => {
                        tracing::debug!("All channels are closed and switch is terminated");
                        break;
                    }
                }
            }
        }.instrument(tracing::info_span!("transport_loop", id=?self.id).or_current()));

        self.state = State::Listening(handle);
    }

    pub async fn query_stream(&self, query_id: QueryId) -> ReceiverStream<CommandEnvelope> {
        let (tx, rx) = mpsc::channel(1);
        let (command, ack_rx) = SubscribeRequest::new(SubscriptionType::Query(query_id), tx);
        self.tx.send(SwitchCommand::Subscribe(command)).await.unwrap();
        ack_rx.await.unwrap();

        ReceiverStream::new(rx)
    }

    pub async fn halt(&self) {
        self.tx.send(SwitchCommand::Halt).await.unwrap();
    }
}

impl Drop for Switch {
    fn drop(&mut self) {
        println!("dropping switch");
        match &self.state {
            State::Listening(handle) => handle.abort(),
            _ => {}
        }
    }
}

#[derive(Default)]
struct QueryCommandRouter {
    routes: HashMap<QueryId, mpsc::Sender<CommandEnvelope>>,
}

impl QueryCommandRouter {
    async fn route(&self, origin: HelperIdentity, data: NetworkEventData) {
        let query_id = data.query_id;
        let sender = self
            .routes
            .get(&query_id)
            .unwrap_or_else(|| panic!("No subscribers for {query_id:?}"));

        sender
            .send(CommandEnvelope {
                origin,
                payload: TransportCommand::NetworkEvent(data),
            })
            .await
            .unwrap();
    }

    fn subscribe(&mut self, query_id: QueryId, sender: mpsc::Sender<CommandEnvelope>) {
        assert!(self.routes.insert(query_id, sender).is_none());
    }
}
