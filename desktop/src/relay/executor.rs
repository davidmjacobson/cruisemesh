use std::sync::Arc;

use anyhow::{bail, Result};
use cruisemesh_core::{
    core_relay_pass_default_budgets, dedupe_hints, recent_presence_hints_for, CoreRelayActionKind,
    CoreRelayContactConfig, CoreRelayEndpointConfig, CoreRelayPass, CoreRelayPassOutcome,
    CoreRelayPassPlan, CoreRelayPassSummary, Identity, MessageStore,
};

use super::RelayHttpClient;

#[derive(Clone, Debug)]
pub struct RelayPassResult {
    pub summary: CoreRelayPassSummary,
    pub sleep_until_ms: Option<i64>,
}

pub async fn run_relay_pass(
    store: Arc<MessageStore>,
    plan: CoreRelayPassPlan,
    http: &RelayHttpClient,
    pass_label: &str,
) -> Result<RelayPassResult> {
    let maximum_actions = plan.budgets.max_requests.saturating_add(8);
    let pass = CoreRelayPass::new(store, plan, pass_label.to_owned());
    let mut action = pass.start(now_ms());
    for _ in 0..maximum_actions {
        let next = match action.kind {
            CoreRelayActionKind::Http { request } => {
                let result = http
                    .execute(action.pass_id.clone(), action.action_id, request)
                    .await;
                pass.resume_http(result)
            }
            CoreRelayActionKind::Sleep { until_ms } => {
                let summary = pass.summary().unwrap_or_else(|| pass.cancel(now_ms()));
                return Ok(RelayPassResult {
                    summary,
                    sleep_until_ms: Some(until_ms),
                });
            }
            CoreRelayActionKind::Finished { summary } => {
                return Ok(RelayPassResult {
                    summary,
                    sleep_until_ms: None,
                });
            }
            CoreRelayActionKind::NotStarted => {
                bail!("relay pass returned NotStarted after start")
            }
        };
        action = next;
    }
    let summary = pass.cancel(now_ms());
    bail!(
        "relay pass exceeded its action guard after {} requests ({:?})",
        summary.requests_issued,
        summary.outcome
    )
}

pub fn build_relay_plan(
    store: &MessageStore,
    identity: &Identity,
    own: Option<CoreRelayEndpointConfig>,
    share_online: bool,
    swept_this_session: bool,
    consecutive_rate_limits: u32,
    quiet_until_ms: i64,
) -> Result<CoreRelayPassPlan> {
    let now = now_ms();
    let contacts = store.list_contacts()?;
    let presence_query = dedupe_hints(
        contacts
            .iter()
            .flat_map(|contact| recent_presence_hints_for(contact.user_id.clone(), now))
            .collect(),
    );
    Ok(CoreRelayPassPlan {
        own,
        contacts: contacts
            .into_iter()
            .map(|contact| CoreRelayContactConfig {
                user_id: contact.user_id,
                relay_url: contact.relay_url,
                relay_token: contact.relay_token,
                endpoint_usable: true,
                endpoint_answering: true,
            })
            .collect(),
        own_user_id: identity.user_id.clone(),
        fetch_hints: store.relay_fetch_hints(identity.user_id.clone(), now)?,
        presence_announce: if share_online {
            recent_presence_hints_for(identity.user_id.clone(), now)
        } else {
            vec![]
        },
        presence_query,
        own_endpoint_changed: false,
        swept_this_session,
        consecutive_rate_limits,
        quiet_until_ms,
        budgets: core_relay_pass_default_budgets(),
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RelayScheduleState {
    pub swept_this_session: bool,
    pub consecutive_rate_limits: u32,
    pub quiet_until_ms: i64,
}

impl RelayScheduleState {
    pub fn observe(&mut self, summary: &CoreRelayPassSummary) {
        if summary.outcome == CoreRelayPassOutcome::Completed {
            self.swept_this_session = true;
            self.consecutive_rate_limits = 0;
            if summary.quiet_until_ms <= now_ms() {
                self.quiet_until_ms = 0;
            }
        } else if summary.outcome == CoreRelayPassOutcome::RateLimited {
            self.consecutive_rate_limits = self.consecutive_rate_limits.saturating_add(1);
        }
        self.quiet_until_ms = self.quiet_until_ms.max(summary.quiet_until_ms);
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
