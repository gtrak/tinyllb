pub mod compressor;
pub mod estimator;
pub mod prompt;
pub mod reconcile;
pub mod segment;
pub mod store;

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use crate::config::ContextPolicy;
use crate::context::estimator::TokenEstimator;
use crate::context::store::TranscriptStore;

/// A background compression job enqueued for the worker (issue 09) to consume.
#[derive(Debug, Clone)]
pub struct CompressionJob {
    pub flow_id: String,
    pub turn_range_start: usize,
    pub turn_range_end: usize,
    pub enqueued_at: std::time::Instant,
}

/// Shared context-compression state: store, estimator, config, per-flow locks,
/// and the compression-job channel sender.
pub struct ContextState {
    pub store: Arc<dyn TranscriptStore>,
    pub estimator: Arc<TokenEstimator>,
    pub config: ContextPolicy,
    flow_locks: dashmap::DashMap<String, Arc<Mutex<()>>>,
    pub compression_tx: Option<mpsc::Sender<CompressionJob>>,
    pub prompt_builder: Arc<crate::context::prompt::PromptBuilder>,
    /// Shared metrics handle for instrumentation.
    pub metrics: Arc<crate::metrics::Metrics>,
}

impl ContextState {
    pub async fn new(
        config: ContextPolicy,
        compression_tx: mpsc::Sender<CompressionJob>,
        metrics: Arc<crate::metrics::Metrics>,
    ) -> anyhow::Result<Self> {
        // Create parent directory for the DB file if it doesn't exist.
        if let Some(parent) = std::path::Path::new(&config.store_path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let store = Arc::new(crate::context::store::SqliteStore::open(&config.store_path).await?);
        let estimator = Arc::new(TokenEstimator::new(config.tokenizer_path.as_deref()));
        let prompt_builder =
            Arc::new(crate::context::prompt::PromptBuilder::new(&config));
        Ok(Self {
            store,
            estimator,
            config,
            flow_locks: dashmap::DashMap::new(),
            compression_tx: Some(compression_tx),
            prompt_builder,
            metrics,
        })
    }

    /// Run `f` while holding the per-flow lock. Serializes concurrent access
    /// to the same transcript (reconcile + compression worker).
    pub async fn with_flow_lock<F, R>(&self, flow_id: &str, f: F) -> R
    where
        F: std::future::Future<Output = R>,
    {
        let lock = self
            .flow_locks
            .entry(flow_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        f.await
    }

    /// Best-effort enqueue of a compression job. Non-blocking; if the channel
    /// is full the job is dropped (it will be re-enqueued on the next request
    /// for that flow). No-op if `config.enabled` is false.
    pub fn trigger_compression(&self, job: CompressionJob) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        let tx = self.compression_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("compression channel closed")
        })?;
        match tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("compression channel full, dropping job (will retry on next request)");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(anyhow::anyhow!("compression channel closed"))
            }
        }
    }

    /// Close the compression channel by dropping the sender.
    /// After calling this, the worker will exit once it processes remaining jobs.
    #[allow(dead_code)]
    pub(crate) fn close_compression_channel(&mut self) {
        self.compression_tx = None;
    }

    /// At startup, enqueue compression jobs for all flows whose estimated
    /// tokens exceed the threshold. Called once after `ContextState` is created.
    pub async fn find_flows_needing_compression(&self) -> anyhow::Result<usize> {
        let flows = self.store.list_flows_over_threshold(self.config.compress_threshold).await?;
        let n = flows.len();
        for flow_id in flows {
            // turn_range_end: 0 is a placeholder — the worker (issue 09)
            // recomputes the range from the live segment.
            let job = CompressionJob {
                flow_id,
                turn_range_start: 0,
                turn_range_end: 0,
                enqueued_at: std::time::Instant::now(),
            };
            let _ = self.trigger_compression(job);
        }
        Ok(n)
    }
}
