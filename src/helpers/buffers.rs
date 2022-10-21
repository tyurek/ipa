use crate::helpers::fabric::{ChannelId, MessageEnvelope};
use crate::protocol::RecordId;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::mem;
use tokio::sync::oneshot;

/// Buffer that keeps messages that must be sent to other helpers
#[derive(Debug)]
pub(super) struct SendBuffer {
    max_capacity: usize,
    inner: HashMap<ChannelId, Vec<MessageEnvelope>>,
}

/// Local buffer for messages that are either awaiting requests to receive them or requests
/// that are pending message reception.
/// TODO: Right now it is backed by a hashmap but `SipHash` (default hasher) performance is not great
/// when protection against collisions is not required, so either use a vector indexed by
/// an offset + record or [xxHash](https://github.com/Cyan4973/xxHash)
#[derive(Debug, Default)]
pub(super) struct ReceiveBuffer {
    inner: HashMap<ChannelId, HashMap<RecordId, ReceiveBufItem>>,
}

#[derive(Debug)]
enum ReceiveBufItem {
    /// There is an outstanding request to receive the message but this helper hasn't seen it yet
    Requested(oneshot::Sender<Box<[u8]>>),
    /// Message has been received but nobody requested it yet
    Received(Box<[u8]>),
}

impl SendBuffer {
    pub fn new(max_channel_capacity: u32) -> Self {
        Self {
            max_capacity: max_channel_capacity as usize,
            inner: HashMap::default(),
        }
    }

    pub fn push(
        &mut self,
        channel_id: ChannelId,
        msg: MessageEnvelope,
    ) -> Option<Vec<MessageEnvelope>> {
        let vec = match self.inner.entry(channel_id) {
            Entry::Occupied(entry) => {
                let vec = entry.into_mut();
                vec.push(msg);

                vec
            }
            Entry::Vacant(entry) => {
                let vec = entry.insert(Vec::with_capacity(self.max_capacity));
                vec.push(msg);

                vec
            }
        };

        if vec.len() >= self.max_capacity {
            let data = mem::replace(vec, Vec::with_capacity(self.max_capacity));
            Some(data)
        } else {
            None
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn remove_random(&mut self) -> (ChannelId, Vec<MessageEnvelope>) {
        assert!(self.len() > 0);

        let channel_id = self.inner.keys().next().unwrap().clone();
        let data = self.inner.remove(&channel_id).unwrap();

        (channel_id, data)
    }
}

impl ReceiveBuffer {
    /// Process request to receive a message with the given `RecordId`.
    pub fn receive_request(
        &mut self,
        channel_id: ChannelId,
        record_id: RecordId,
        sender: oneshot::Sender<Box<[u8]>>,
    ) {
        match self.inner.entry(channel_id).or_default().entry(record_id) {
            Entry::Occupied(entry) => match entry.remove() {
                ReceiveBufItem::Requested(_) => {
                    panic!("More than one request to receive a message for {record_id:?}");
                }
                ReceiveBufItem::Received(payload) => {
                    sender.send(payload).unwrap_or_else(|_| {
                        tracing::warn!("No listener for message {record_id:?}");
                    });
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(ReceiveBufItem::Requested(sender));
            }
        }
    }

    /// Process message that has been received
    pub fn receive_messages(&mut self, channel_id: &ChannelId, messages: Vec<MessageEnvelope>) {
        for msg in messages {
            match self
                .inner
                .entry(channel_id.clone())
                .or_default()
                .entry(msg.record_id)
            {
                Entry::Occupied(entry) => match entry.remove() {
                    ReceiveBufItem::Requested(s) => {
                        s.send(msg.payload).unwrap_or_else(|_| {
                            tracing::warn!("No listener for message {:?}", msg.record_id);
                        });
                    }
                    ReceiveBufItem::Received(_) => {
                        panic!("Duplicate message for the same record {:?}", msg.record_id);
                    }
                },
                Entry::Vacant(entry) => {
                    entry.insert(ReceiveBufItem::Received(msg.payload));
                }
            }
        }
    }
}