//!
//! This module contains implementations and traits that enable protocols to communicate with
//! each other. In order for helpers to send messages, they need to know the destination. In some
//! cases this might be the exact address of helper host/instance (for example IP address), but
//! in many situations MPC helpers simply need to be able to send messages to the
//! corresponding helper without needing to know the exact location - this is what this module
//! enables MPC protocols to do.
//!
use crate::{
    helpers::buffers::ReceiveBuffer,
    helpers::error::Error,
    helpers::network::{ChannelId, MessageEnvelope, Network},
    helpers::Role,
    protocol::{RecordId, Step},
};

use crate::ff::{Field, Int};
use crate::helpers::buffers::{SendBuffer, SendBufferConfig};
use crate::helpers::{MessagePayload, MESSAGE_PAYLOAD_SIZE_BYTES};
use crate::task::JoinHandle;
use ::tokio::sync::{mpsc, oneshot};
use ::tokio::time::Instant;
use futures::SinkExt;
use futures::StreamExt;
use std::fmt::{Debug, Formatter};
use std::time::Duration;
use std::{io, panic};
use tinyvec::array_vec;
use tracing::Instrument;

#[cfg(all(feature = "shuttle", test))]
use shuttle::future as tokio;

/// Trait for messages sent between helpers
pub trait Message: Debug + Send + Sized + 'static {
    /// Required number of bytes to store this message on disk/network
    const SIZE_IN_BYTES: u32;

    /// Deserialize message from a sequence of bytes.
    ///
    /// ## Errors
    /// Returns an error if the provided buffer does not have enough bytes to read (EOF).
    fn deserialize(buf: &mut [u8]) -> io::Result<Self>;

    /// Serialize this message to a mutable slice. Implementations need to ensure `buf` has enough
    /// capacity to store this message.
    ///
    /// ## Errors
    /// Returns an error if `buf` does not have enough capacity to store at least `SIZE_IN_BYTES` more
    /// data.
    fn serialize(self, buf: &mut [u8]) -> io::Result<()>;
}

/// Any field value can be send as a message
impl<F: Field> Message for F {
    const SIZE_IN_BYTES: u32 = F::Integer::BITS / 8;

    fn deserialize(buf: &mut [u8]) -> io::Result<Self> {
        <F as Field>::deserialize(buf)
    }

    fn serialize(self, buf: &mut [u8]) -> io::Result<()> {
        <F as Field>::serialize(&self, buf)
    }
}

/// Entry point to the messaging layer managing communication channels for protocols and provides
/// the ability to send and receive messages from helper peers. Protocols request communication
/// channels to be open by calling `get_channel`, after that it is possible to send messages
/// through the channel end and request a given message type from helper peer.
///
/// Gateways are generic over `Network` meaning they can operate on top of in-memory communication
/// channels and real network.
///
/// ### Implementation details
/// Gateway, when created, runs an event loop in a dedicated tokio task that pulls the messages
/// from the networking layer and attempts to fulfil the outstanding requests to receive them.
/// If `receive` method on the channel has never been called, it puts the message to the local
/// buffer and keeps it there until such request is made by the protocol.
/// TODO: limit the size of the buffer and only pull messages when there is enough capacity
#[derive(Debug)]
pub struct Gateway {
    /// Sender end of the channel to send requests to receive messages from peers.
    tx: mpsc::Sender<ReceiveRequest>,
    envelope_tx: mpsc::Sender<SendRequest>,
    control_handle: JoinHandle<()>,
}

pub(super) type SendRequest = (ChannelId, MessageEnvelope);

/// Channel end
#[derive(Debug)]
pub struct Mesh<'a, 'b> {
    gateway: &'a Gateway,
    step: &'b Step,
}

pub(super) struct ReceiveRequest {
    pub channel_id: ChannelId,
    pub record_id: RecordId,
    pub sender: oneshot::Sender<MessagePayload>,
}

impl Mesh<'_, '_> {
    /// Send a given message to the destination. This method will not return until the message
    /// is delivered to the `Network`.
    ///
    /// # Errors
    /// Returns an error if it fails to send the message or if there is a serialization error.
    pub async fn send<T: Message>(
        &self,
        dest: Role,
        record_id: RecordId,
        msg: T,
    ) -> Result<(), Error> {
        if T::SIZE_IN_BYTES as usize > MESSAGE_PAYLOAD_SIZE_BYTES {
            Err(Error::serialization_error::<String>(record_id,
                                                     self.step,
                                                     format!("Message {msg:?} exceeds the maximum size allowed: {MESSAGE_PAYLOAD_SIZE_BYTES}"))
            )?;
        }

        let mut payload = array_vec![0; MESSAGE_PAYLOAD_SIZE_BYTES];
        msg.serialize(&mut payload)
            .map_err(|e| Error::serialization_error(record_id, self.step, e))?;

        let envelope = MessageEnvelope { record_id, payload };

        self.gateway
            .send(ChannelId::new(dest, self.step.clone()), envelope)
            .await
    }

    /// Receive a message that is associated with the given record id.
    ///
    /// # Errors
    /// Returns an error if it fails to receive the message or if a deserialization error occurred
    pub async fn receive<T: Message>(&self, source: Role, record_id: RecordId) -> Result<T, Error> {
        let mut payload = self
            .gateway
            .receive(ChannelId::new(source, self.step.clone()), record_id)
            .await?;

        let obj = T::deserialize(&mut payload)
            .map_err(|e| Error::serialization_error(record_id, self.step, e))?;

        Ok(obj)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GatewayConfig {
    /// Configuration for send buffers. See `SendBufferConfig` for more details
    pub send_buffer_config: SendBufferConfig,
}

impl Gateway {
    pub fn new<N: Network>(role: Role, network: &N, config: GatewayConfig) -> Self {
        let (recv_tx, mut recv_rx) = mpsc::channel::<ReceiveRequest>(1);
        let (send_tx, mut send_rx) = mpsc::channel::<SendRequest>(1);
        let mut message_stream = network.recv_stream();
        let mut network_sink = network.sink();

        let control_handle = tokio::spawn(async move {
            const INTERVAL: Duration = Duration::from_secs(10);

            let mut receive_buf = ReceiveBuffer::default();
            let mut send_buf = SendBuffer::new(config.send_buffer_config);

            let sleep = ::tokio::time::sleep(INTERVAL);
            ::tokio::pin!(sleep);

            loop {
                // Make a random choice what to process next:
                // * Receive a message from another helper
                // * Handle the request to receive a message from another helper
                // * Send a message
                ::tokio::select! {
                    Some(receive_request) = recv_rx.recv() => {
                        tracing::trace!("new {:?}", receive_request);
                        receive_buf.receive_request(receive_request.channel_id, receive_request.record_id, receive_request.sender);
                    }
                    Some((channel_id, messages)) = message_stream.next() => {
                        tracing::trace!("received {} bytes from {:?}", messages.len(), channel_id);
                        receive_buf.receive_messages(&channel_id, &messages);
                    }
                    Some(send_req) = send_rx.recv() => {
                        tracing::trace!("new SendRequest({:?})", send_req);
                        send_message::<N>(&mut network_sink, &mut send_buf, send_req).await;
                    }
                    _ = &mut sleep => {
                        #[cfg(debug_assertions)]
                        print_state(role, &send_buf, &receive_buf);
                    }
                    else => {
                        tracing::debug!("All channels are closed and event loop is terminated");
                        break;
                    }
                }

                // reset the timer on every action
                sleep.as_mut().reset(Instant::now() + INTERVAL);
            }
        }.instrument(tracing::info_span!("gateway_loop", role=?role)));

        Self {
            tx: recv_tx,
            envelope_tx: send_tx,
            control_handle,
        }
    }

    /// Create or return an existing channel for a given step. Protocols can send messages to
    /// any helper through this channel (see `Mesh` interface for details).
    ///
    /// This method makes no guarantee that the communication channel will actually be established
    /// between this helper and every other one. The actual connection may be created only when
    /// `Mesh::send` or `Mesh::receive` methods are called.
    #[must_use]
    pub fn mesh<'a, 'b>(&'a self, step: &'b Step) -> Mesh<'a, 'b> {
        Mesh {
            gateway: self,
            step,
        }
    }

    /// Join the control loop task and wait until its completed.
    ///
    /// ## Panics
    /// if control loop task panicked, the panic will be propagated to this thread
    #[cfg(not(feature = "shuttle"))]
    pub async fn join(self) {
        self.control_handle
            .await
            .map_err(|e| {
                if e.is_panic() {
                    panic::resume_unwind(e.into_panic())
                } else {
                    "Task cancelled".to_string()
                }
            })
            .unwrap();
    }

    async fn receive(
        &self,
        channel_id: ChannelId,
        record_id: RecordId,
    ) -> Result<MessagePayload, Error> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ReceiveRequest {
                channel_id: channel_id.clone(),
                record_id,
                sender: tx,
            })
            .await?;

        rx.await
            .map_err(|e| Error::receive_error(channel_id.role, e))
    }

    async fn send(&self, id: ChannelId, env: MessageEnvelope) -> Result<(), Error> {
        Ok(self.envelope_tx.send((id, env)).await?)
    }
}

#[cfg(feature = "shuttle")]
impl Drop for Gateway {
    fn drop(&mut self) {
        self.control_handle.abort();
    }
}

impl Debug for ReceiveRequest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ReceiveRequest({:?}, {:?})",
            self.channel_id, self.record_id
        )
    }
}

async fn send_message<N: Network>(sink: &mut N::Sink, buf: &mut SendBuffer, req: SendRequest) {
    let (channel_id, msg) = req;
    match buf.push(&channel_id, &msg) {
        Ok(Some(buf_to_send)) => {
            tracing::trace!("sending {} bytes to {:?}", buf_to_send.len(), &channel_id);
            sink.send((channel_id, buf_to_send))
                .await
                .expect("Failed to send data to the network");
        }
        Ok(None) => {}
        Err(err) => panic!("failed to send to the {channel_id:?}: {err}"),
    };
}

#[cfg(debug_assertions)]
fn print_state(role: Role, send_buf: &SendBuffer, receive_buf: &ReceiveBuffer) {
    let send_tasks_waiting = send_buf.waiting();
    let receive_tasks_waiting = receive_buf.waiting();
    if !send_tasks_waiting.is_empty() || !receive_tasks_waiting.is_empty() {
        tracing::error!(
            "List of tasks pending completion on {role:?}:\
        \nwaiting to send: {:?},\
        \nwaiting to receive: {:?}",
            send_buf.waiting(),
            receive_buf.waiting()
        );
    }
}

#[cfg(all(test, not(feature = "shuttle")))]
mod tests {
    use crate::ff::Fp31;
    use crate::helpers::Role;
    use crate::protocol::context::Context;
    use crate::protocol::{QueryId, RecordId, Step};
    use crate::test_fixture::{TestWorld, TestWorldConfig};

    #[tokio::test]
    pub async fn handles_reordering() {
        let mut config = TestWorldConfig::default();
        config.gateway_config.send_buffer_config.items_in_batch = 1; // Send every record
        config.gateway_config.send_buffer_config.batch_count = 3; // keep 3 at a time

        let world = Box::leak(Box::new(TestWorld::new_with(QueryId, config)));
        let contexts = world.contexts::<Fp31>();
        let sender_ctx = contexts[0].narrow("reordering-test");
        let recv_ctx = contexts[1].narrow("reordering-test");

        // send record 1 first and wait for confirmation before sending record 0.
        // when gateway received record 0 it triggers flush so it must make sure record 1 is also
        // sent (same batch or different does not matter here)
        tokio::spawn(async move {
            let channel = sender_ctx.mesh();
            channel
                .send(Role::H2, RecordId::from(1), Fp31::from(1_u128))
                .await
                .unwrap();
            channel
                .send(Role::H2, RecordId::from(0), Fp31::from(0_u128))
                .await
                .unwrap();
        });

        // intentionally ignoring record 0 here
        let v: Fp31 = recv_ctx
            .mesh()
            .receive(Role::H1, RecordId::from(1))
            .await
            .unwrap();
        assert_eq!(Fp31::from(1_u128), v);
    }

    #[tokio::test]
    #[should_panic(expected = "Record RecordId(1) has been received twice")]
    async fn duplicate_message() {
        let world = TestWorld::new(QueryId);
        let [g1, _, _] = world.gateways;
        let (v1, v2) = (Fp31::from(1u128), Fp31::from(2u128));
        let peer = Role::H2;
        let record_id = 1.into();
        let step = Step::default();
        let channel = g1.mesh(&step);

        channel.send(peer, record_id, v1).await.unwrap();
        channel.send(peer, record_id, v2).await.unwrap();

        g1.join().await;
    }
}
