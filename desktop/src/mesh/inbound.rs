use std::{sync::Arc, thread};

use anyhow::{bail, Context, Result};
use cruisemesh_core::{
    parse_frame, CoreInboundDisposition, CoreInboundSource, Frame, Identity, MessageArrival,
    MessageStore, SeenIds, DEFAULT_HOP_TTL,
};
use tokio::sync::{mpsc, oneshot};

use crate::{lan::endpoint_cache::EndpointCache, mesh::delivery::DeliveryDispatcher};

#[derive(Clone, Debug)]
pub struct ProcessedFrame {
    pub disposition: CoreInboundDisposition,
    pub relay_frame: Option<Vec<u8>>,
    pub carried: bool,
}

struct InboundJob {
    source: CoreInboundSource,
    frame: Vec<u8>,
    now_ms: i64,
    response: oneshot::Sender<Result<ProcessedFrame>>,
}

#[derive(Clone)]
pub struct InboundExecutor {
    sender: mpsc::Sender<InboundJob>,
    seen: Arc<SeenIds>,
}

impl InboundExecutor {
    pub fn start(
        store: Arc<MessageStore>,
        identity: Identity,
        endpoints: EndpointCache,
    ) -> Result<Self> {
        Self::start_with_discovery(store, identity, endpoints, Arc::new(|| (true, 0)))
    }

    pub fn start_with_discovery(
        store: Arc<MessageStore>,
        identity: Identity,
        endpoints: EndpointCache,
        discovery: crate::mesh::delivery::DiscoveryPolicy,
    ) -> Result<Self> {
        let seen = Arc::new(SeenIds::new());
        for msg_id in store.non_relay_carried_msg_ids(512)? {
            seen.record(msg_id);
        }
        for msg_id in store.recent_consumed_msg_ids(512)? {
            seen.record(msg_id);
        }

        let (sender, mut receiver) = mpsc::channel::<InboundJob>(256);
        let worker_seen = seen.clone();
        thread::Builder::new()
            .name("cruisemesh-store".into())
            .spawn(move || {
                let dispatcher =
                    DeliveryDispatcher::new(store.clone(), identity.clone(), endpoints, discovery);
                while let Some(job) = receiver.blocking_recv() {
                    let result = process_one(
                        &store,
                        &identity,
                        &worker_seen,
                        &dispatcher,
                        job.source,
                        job.frame,
                        job.now_ms,
                    );
                    let _ = job.response.send(result);
                }
            })
            .context("failed to start serialized store executor")?;
        Ok(Self { sender, seen })
    }

    pub async fn process(
        &self,
        source: CoreInboundSource,
        frame: Vec<u8>,
        now_ms: i64,
    ) -> Result<ProcessedFrame> {
        let (response, receive) = oneshot::channel();
        self.sender
            .send(InboundJob {
                source,
                frame,
                now_ms,
                response,
            })
            .await
            .context("store executor stopped")?;
        receive.await.context("store executor dropped response")?
    }

    pub fn record_authored(&self, msg_id: Vec<u8>) {
        self.seen.record(msg_id);
    }

    pub async fn drain_relay_sourced(&self, store: &MessageStore, now_ms: i64) -> Result<u32> {
        let mut consumed = 0_u32;
        for envelope in store.relay_sourced_carried_envelopes(256, now_ms)? {
            let frame = cruisemesh_core::encode_envelope_frame(
                envelope.msg_id.clone(),
                envelope.hop_ttl,
                envelope.expiry,
                envelope.recipient_hint,
                envelope.sealed,
            );
            let outcome = self
                .process(CoreInboundSource::Relay, frame, now_ms)
                .await?;
            if outcome.disposition == CoreInboundDisposition::Consumed {
                store.remove_carried_envelope(envelope.msg_id)?;
                consumed = consumed.saturating_add(1);
            }
        }
        Ok(consumed)
    }
}

fn process_one(
    store: &Arc<MessageStore>,
    identity: &Identity,
    seen: &Arc<SeenIds>,
    dispatcher: &DeliveryDispatcher,
    source: CoreInboundSource,
    frame: Vec<u8>,
    now_ms: i64,
) -> Result<ProcessedFrame> {
    let hop_ttl = match parse_frame(frame.clone()) {
        Ok(Frame::Envelope { hop_ttl, .. }) => hop_ttl,
        _ => 0,
    };
    let outcome =
        store.process_inbound_frame(identity.clone(), seen.clone(), source, frame, now_ms)?;
    if outcome.delivered_payloads.len() > 1 {
        bail!("core returned more than one delivered payload");
    }
    if let Some(payload) = outcome.delivered_payloads.first() {
        let sender = outcome
            .delivered_sender
            .clone()
            .context("delivered payload has no verified sender")?;
        let commit = outcome
            .commit
            .clone()
            .context("delivered payload has no commit token")?;
        let transport = match source {
            CoreInboundSource::Relay => 2,
            CoreInboundSource::Mesh if hop_ttl < DEFAULT_HOP_TTL => 4,
            CoreInboundSource::Mesh => 3,
        };
        dispatcher.deliver(
            sender,
            payload.clone(),
            &commit,
            MessageArrival {
                transport,
                hops_taken: DEFAULT_HOP_TTL.saturating_sub(hop_ttl),
                received_at: now_ms,
            },
        )?;
        store.core_commit_inbound_delivery(seen.clone(), commit);
    }
    Ok(ProcessedFrame {
        disposition: outcome.disposition,
        relay_frame: outcome.relay_frame,
        carried: outcome.carried,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cruisemesh_core::{
        create_shared_friend_card, generate_identity, make_friend_card,
        make_shared_friend_request_payload, Contact, CoreRelayEnvelopeDisposition,
        CoreRelayFetchedEnvelope, KIND_FRIEND_DIRECTORY,
    };

    const NOW: i64 = 1_700_000_000_000;

    fn contact(identity: &Identity, name: &str) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: name.into(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    fn executor() -> (
        tempfile::TempDir,
        Arc<MessageStore>,
        Identity,
        InboundExecutor,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            MessageStore::open(temp.path().join("messages.db").to_string_lossy().into()).unwrap(),
        );
        let helper = generate_identity();
        let executor = InboundExecutor::start(
            store.clone(),
            helper.clone(),
            EndpointCache::new(temp.path().join("endpoints.json")),
        )
        .unwrap();
        (temp, store, helper, executor)
    }

    #[tokio::test]
    async fn direct_friend_request_auto_imports_then_commits() {
        let (_temp, helper_store, helper, executor) = executor();
        let phone = generate_identity();
        let phone_store = MessageStore::open(":memory:".into()).unwrap();
        let card = make_friend_card("Emma".into(), phone.clone(), None, None).unwrap();
        let authored = phone_store
            .author_friend_request(phone.clone(), contact(&helper, "Cabin PC"), card, NOW)
            .unwrap();

        let result = executor
            .process(CoreInboundSource::Relay, authored.frame, NOW)
            .await
            .unwrap();
        assert_eq!(result.disposition, CoreInboundDisposition::Consumed);
        assert_eq!(
            helper_store
                .get_contact(phone.user_id.clone())
                .unwrap()
                .unwrap()
                .name,
            "Emma"
        );
    }

    #[tokio::test]
    async fn shared_tail_does_not_create_a_contact() {
        let (_temp, helper_store, helper, executor) = executor();
        let phone = generate_identity();
        let sharer = generate_identity();
        let phone_store = MessageStore::open(":memory:".into()).unwrap();
        let helper_card_json =
            make_friend_card("Cabin PC".into(), helper.clone(), None, None).unwrap();
        let helper_card = cruisemesh_core::parse_friend_card(helper_card_json).unwrap();
        let shared = create_shared_friend_card(sharer, helper_card, 1, NOW).unwrap();
        let phone_card = make_friend_card("Emma".into(), phone.clone(), None, None).unwrap();
        let tailed = make_shared_friend_request_payload(phone_card, shared).unwrap();
        let authored = phone_store
            .author_friend_request(phone.clone(), contact(&helper, "Cabin PC"), tailed, NOW)
            .unwrap();

        executor
            .process(CoreInboundSource::Relay, authored.frame, NOW)
            .await
            .unwrap();
        assert!(helper_store.get_contact(phone.user_id).unwrap().is_none());
    }

    #[tokio::test]
    async fn unauthorized_pairwise_control_is_consumed_instead_of_crashing() {
        let (_temp, _helper_store, helper, executor) = executor();
        let stranger = generate_identity();
        let sender_store = MessageStore::open(":memory:".into()).unwrap();
        let authored = sender_store
            .author_pairwise_message(
                stranger.clone(),
                contact(&helper, "Cabin PC"),
                KIND_FRIEND_DIRECTORY,
                b"not yet a contact".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        let frame = authored.frame;
        assert_eq!(
            executor
                .process(CoreInboundSource::Mesh, frame.clone(), NOW)
                .await
                .unwrap()
                .disposition,
            CoreInboundDisposition::Consumed
        );
        assert_eq!(
            executor
                .process(CoreInboundSource::Mesh, frame, NOW + 1)
                .await
                .unwrap()
                .disposition,
            CoreInboundDisposition::Seen
        );
    }

    #[tokio::test]
    async fn pure_mule_traffic_never_becomes_a_relay_ack() {
        let (_temp, helper_store, helper, executor) = executor();
        let sender = generate_identity();
        let recipient = generate_identity();
        let sender_store = MessageStore::open(":memory:".into()).unwrap();
        let authored = sender_store
            .author_pairwise_message(
                sender.clone(),
                contact(&recipient, "Emma"),
                cruisemesh_core::KIND_TEXT,
                b"for Emma".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        let result = executor
            .process(CoreInboundSource::Mesh, authored.frame, NOW)
            .await
            .unwrap();
        assert_eq!(result.disposition, CoreInboundDisposition::Carried);
        let acknowledgements = helper_store
            .core_relay_ack_ids_with_consumed(
                vec![CoreRelayEnvelopeDisposition {
                    relay_id: 41,
                    msg_id: authored.envelope.msg_id,
                    disposition: result.disposition,
                    recipient_hint: authored.envelope.recipient_hint,
                }],
                helper.user_id,
                NOW,
            )
            .unwrap();
        assert!(acknowledgements.is_empty());
    }

    #[tokio::test]
    async fn relay_sourced_friend_request_is_drained_through_shared_inbound() {
        let (_temp, helper_store, helper, executor) = executor();
        let phone = generate_identity();
        let phone_store = MessageStore::open(":memory:".into()).unwrap();

        // Phones may publish their directory before the onboarding friend
        // request. Kind 6 from this not-yet-contact must be a terminal drop,
        // not an error that tears down the helper at startup.
        let directory = phone_store
            .author_pairwise_message(
                phone.clone(),
                contact(&helper, "Cabin PC"),
                KIND_FRIEND_DIRECTORY,
                b"directory".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        let Frame::Envelope {
            msg_id,
            hop_ttl,
            recipient_hint,
            sealed,
            expiry,
        } = parse_frame(directory.frame).unwrap()
        else {
            panic!("authoring did not return an envelope")
        };
        helper_store
            .ingest_relay_page(
                vec![CoreRelayFetchedEnvelope {
                    id: 1,
                    msg_id,
                    hop_ttl,
                    recipient_hint,
                    sealed,
                    expiry_ms: expiry,
                }],
                NOW,
                Some("test".into()),
                1,
            )
            .unwrap();
        assert_eq!(
            executor
                .drain_relay_sourced(&helper_store, NOW)
                .await
                .unwrap(),
            1
        );
        assert!(helper_store
            .get_contact(phone.user_id.clone())
            .unwrap()
            .is_none());

        let card = make_friend_card("Emma".into(), phone.clone(), None, None).unwrap();
        let authored = phone_store
            .author_friend_request(phone.clone(), contact(&helper, "Cabin PC"), card, NOW)
            .unwrap();
        let Frame::Envelope {
            msg_id,
            hop_ttl,
            recipient_hint,
            sealed,
            expiry,
        } = parse_frame(authored.frame).unwrap()
        else {
            panic!("authoring did not return an envelope")
        };
        helper_store
            .ingest_relay_page(
                vec![CoreRelayFetchedEnvelope {
                    id: 2,
                    msg_id: msg_id.clone(),
                    hop_ttl,
                    recipient_hint,
                    sealed,
                    expiry_ms: expiry,
                }],
                NOW,
                Some("test".into()),
                2,
            )
            .unwrap();

        assert_eq!(
            executor
                .drain_relay_sourced(&helper_store, NOW)
                .await
                .unwrap(),
            1
        );
        assert!(helper_store.get_contact(phone.user_id).unwrap().is_some());
        assert!(helper_store
            .relay_sourced_carried_envelopes(10, NOW)
            .unwrap()
            .is_empty());
    }
}
