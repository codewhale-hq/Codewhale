use crate::approval_log::ApprovalReceipt;
use anyhow::{Context as _, Result, bail};
use std::sync::Arc;

#[derive(Clone)]
pub struct NativeApprovalPublication(Arc<PublicationState>);

struct PublicationState {
    tool_id: String,
    state: parking_lot::Mutex<PublicationValue>,
    changed: tokio::sync::watch::Sender<u64>,
}

#[derive(Default)]
struct PublicationValue {
    bound: bool,
    timeout: Option<std::time::Duration>,
    receipt: Option<Result<ApprovalReceipt, String>>,
    publication: Option<Result<(), String>>,
    #[cfg(any(test, feature = "test-support"))]
    mock_controls: Option<(
        tokio_util::sync::CancellationToken,
        Option<tokio::time::Instant>,
    )>,
}

pub(crate) struct ApprovalProducer {
    source: NativeApprovalPublication,
    current: Option<Arc<parking_lot::Mutex<Option<NativeApprovalPublication>>>>,
}

pub(crate) struct ApprovalPublicationBinding {
    source: NativeApprovalPublication,
}

impl std::fmt::Debug for NativeApprovalPublication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeApprovalPublication(held Core approval)")
    }
}

impl NativeApprovalPublication {
    pub(crate) fn new(tool_id: &str) -> (Self, ApprovalProducer) {
        let (changed, _) = tokio::sync::watch::channel(0);
        let source = Self(Arc::new(PublicationState {
            tool_id: tool_id.to_owned(),
            state: parking_lot::Mutex::new(PublicationValue::default()),
            changed,
        }));
        let producer = ApprovalProducer {
            source: source.clone(),
            current: None,
        };
        (source, producer)
    }

    pub(crate) fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn is_for(&self, tool_id: &str) -> bool {
        self.0.tool_id == tool_id
    }

    pub(crate) fn bind(&self, tool_id: &str) -> Result<ApprovalPublicationBinding> {
        anyhow::ensure!(
            self.0.tool_id == tool_id,
            "Core approval source identity changed"
        );
        let mut state = self.0.state.lock();
        anyhow::ensure!(
            !state.bound,
            "Core approval already has its publication owner"
        );
        anyhow::ensure!(
            match state.receipt.as_ref() {
                None => true,
                Some(Ok(ApprovalReceipt::Decided { outcome, .. })) => matches!(
                    outcome,
                    crate::approval_log::ApprovalOutcome::Denied
                        | crate::approval_log::ApprovalOutcome::Timeout
                        | crate::approval_log::ApprovalOutcome::Cancelled
                        | crate::approval_log::ApprovalOutcome::Unavailable
                ),
                _ => false,
            },
            "Core approval already settled before publication binding"
        );
        state.bound = true;
        Ok(ApprovalPublicationBinding {
            source: self.clone(),
        })
    }

    pub(crate) fn set_timeout(&self, timeout: Option<std::time::Duration>) {
        self.0.state.lock().timeout = timeout;
    }

    pub(crate) fn timeout_seconds(&self) -> Option<u64> {
        self.0.state.lock().timeout.map(|timeout| timeout.as_secs())
    }

    fn changed(&self) {
        self.0
            .changed
            .send_modify(|epoch| *epoch = epoch.saturating_add(1));
    }

    pub(crate) async fn actual(&self) -> Result<ApprovalReceipt> {
        let mut changed = self.0.changed.subscribe();
        loop {
            if let Some(receipt) = self.0.state.lock().receipt.clone() {
                return receipt.map_err(anyhow::Error::msg);
            }
            changed
                .changed()
                .await
                .context("Core approval outcome channel closed")?;
        }
    }

    pub(crate) async fn published(&self) -> Result<ApprovalReceipt> {
        let mut changed = self.0.changed.subscribe();
        loop {
            let (receipt, publication, bound) = {
                let state = self.0.state.lock();
                (
                    state.receipt.clone(),
                    state.publication.clone(),
                    state.bound,
                )
            };
            if let Some(Err(error)) = receipt.as_ref() {
                bail!("Core approval outcome is unconfirmed: {error}");
            }
            if let Some(Err(error)) = publication.as_ref() {
                bail!("Core approval publication is unconfirmed: {error}");
            }
            if let Some(receipt) = receipt {
                let receipt = receipt.map_err(anyhow::Error::msg)?;
                if !bound || matches!(publication, Some(Ok(()))) {
                    return Ok(receipt);
                }
            }
            changed
                .changed()
                .await
                .context("Core approval publication channel closed")?;
        }
    }

    pub(crate) async fn commit(&self, receipt: ApprovalReceipt) -> Result<()> {
        anyhow::ensure!(
            receipt.approval_id() == self.0.tool_id,
            "Core approval receipt identity changed"
        );
        {
            let mut state = self.0.state.lock();
            anyhow::ensure!(
                state.receipt.is_none(),
                "Core approval already has its winning receipt"
            );
            state.receipt = Some(Ok(receipt));
        }
        self.changed();
        self.published().await?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn set_mock_controls(
        &self,
        cancel: tokio_util::sync::CancellationToken,
        timeout: Option<std::time::Duration>,
    ) {
        let mut state = self.0.state.lock();
        state.timeout = timeout;
        state.mock_controls = Some((
            cancel,
            timeout.map(|timeout| tokio::time::Instant::now() + timeout),
        ));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn commit_mock_outcome(&self, receipt: ApprovalReceipt) -> Result<bool> {
        anyhow::ensure!(
            receipt.approval_id() == self.0.tool_id,
            "mock Core approval identity changed"
        );
        let mut state = self.0.state.lock();
        if state.receipt.is_some() {
            return Ok(false);
        }
        let receipt = match state.mock_controls.as_ref() {
            Some((cancel, _)) if cancel.is_cancelled() => {
                crate::approval_log::ApprovalReceipt::decided_with(
                    &self.0.tool_id,
                    crate::approval_log::ApprovalOutcome::Cancelled,
                    Some(crate::approval_log::ApprovalDecider::Host),
                )
            }
            Some((_, Some(deadline))) if tokio::time::Instant::now() >= *deadline => {
                crate::approval_log::ApprovalReceipt::decided_with(
                    &self.0.tool_id,
                    crate::approval_log::ApprovalOutcome::Timeout,
                    None,
                )
            }
            _ => receipt,
        };
        state.receipt = Some(Ok(receipt));
        drop(state);
        self.changed();
        Ok(true)
    }

    pub(crate) fn acknowledge(&self, result: Result<(), String>) -> Result<()> {
        let mut state = self.0.state.lock();
        if result.is_ok() {
            anyhow::ensure!(
                matches!(state.receipt.as_ref(), Some(Ok(_))),
                "Core approval receipt is not committed"
            );
        }
        if let Some(previous) = state.publication.as_ref() {
            return previous.clone().map_err(anyhow::Error::msg);
        }
        state.publication = Some(result);
        drop(state);
        self.changed();
        Ok(())
    }

    pub(crate) fn fail_native(&self, error: String) {
        let mut state = self.0.state.lock();
        if state.receipt.is_none() {
            state.receipt = Some(Err(error));
        }
        drop(state);
        self.changed();
    }

    fn close(&self) {
        let mut state = self.0.state.lock();
        if state.receipt.is_none() {
            state.receipt = Some(Err(
                "Core approval ended without a confirmed durable outcome".into(),
            ));
        }
        if state.bound && state.publication.is_none() {
            state.publication = Some(Err(
                "Core approval publication owner ended before acknowledgement".into(),
            ));
        }
        drop(state);
        self.changed();
    }
}

impl ApprovalProducer {
    pub(crate) fn install(
        &mut self,
        current: Arc<parking_lot::Mutex<Option<NativeApprovalPublication>>>,
    ) -> Result<()> {
        let mut slot = current.lock();
        anyhow::ensure!(slot.is_none(), "Core already has a current approval waiter");
        *slot = Some(self.source.clone());
        drop(slot);
        self.current = Some(current);
        Ok(())
    }
}

impl Drop for ApprovalProducer {
    fn drop(&mut self) {
        self.source.close();
        if let Some(current) = self.current.as_ref() {
            let mut slot = current.lock();
            if slot
                .as_ref()
                .is_some_and(|source| source.same_request(&self.source))
            {
                *slot = None;
            }
        }
    }
}

impl Drop for ApprovalPublicationBinding {
    fn drop(&mut self) {
        let _ = self.source.acknowledge(Err(
            "Runtime approval projection ended before durable acknowledgement".into(),
        ));
    }
}
