/// Priority request queue for LLM classification (Sprint 052, Phase 2).
///
/// Routes classification requests through priority-ordered channels so voice
/// requests are served before interactive, and interactive before background.
/// Workers consume from the channels using `tokio::select! { biased; }`.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

use ygg_domain::sdr::Sdr;

use crate::llm_router::{LlmClassification, LlmRouterClient};
use crate::sdr_router::SdrClassification;

// ─────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────

/// Request priority tier.  Higher numeric value = higher priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RequestPriority {
    Background = 0,
    Interactive = 1,
    Voice = 2,
}

/// A queued classification request.
pub struct ClassificationRequest {
    pub message: String,
    pub query_sdr: Option<Sdr>,
    pub sdr_suggestion: Option<SdrClassification>,
    pub response_tx: oneshot::Sender<Option<LlmClassification>>,
    pub enqueued_at: Instant,
}

// ─────────────────────────────────────────────────────────────────
// RequestQueue
// ─────────────────────────────────────────────────────────────────

/// Priority-ordered request queue backed by three bounded mpsc channels.
#[derive(Clone)]
pub struct RequestQueue {
    voice_tx: mpsc::Sender<ClassificationRequest>,
    interactive_tx: mpsc::Sender<ClassificationRequest>,
    background_tx: mpsc::Sender<ClassificationRequest>,
    depth: Arc<[AtomicUsize; 3]>,
}

impl RequestQueue {
    /// Create a new queue and return `(queue_handle, receiver_bundle)`.
    ///
    /// `queue_size` sets the bounded capacity for each priority channel.
    pub fn new(queue_size: usize) -> (Self, QueueReceivers) {
        let (voice_tx, voice_rx) = mpsc::channel(queue_size.max(1));
        let (interactive_tx, interactive_rx) = mpsc::channel(queue_size.max(1));
        let (background_tx, background_rx) = mpsc::channel(queue_size.max(1));

        let depth = Arc::new([
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        ]);

        let queue = Self {
            voice_tx,
            interactive_tx,
            background_tx,
            depth,
        };

        let receivers = QueueReceivers {
            voice_rx,
            interactive_rx,
            background_rx,
        };

        (queue, receivers)
    }

    /// Submit a classification request.
    ///
    /// Returns a oneshot receiver for the result.  If the channel for the given
    /// priority is full, the oneshot immediately receives `None` (back-pressure).
    pub fn submit(
        &self,
        message: String,
        query_sdr: Option<Sdr>,
        sdr_suggestion: Option<SdrClassification>,
        priority: RequestPriority,
    ) -> oneshot::Receiver<Option<LlmClassification>> {
        let (response_tx, response_rx) = oneshot::channel();
        let req = ClassificationRequest {
            message,
            query_sdr,
            sdr_suggestion,
            response_tx,
            enqueued_at: Instant::now(),
        };

        let tx = match priority {
            RequestPriority::Voice => &self.voice_tx,
            RequestPriority::Interactive => &self.interactive_tx,
            RequestPriority::Background => &self.background_tx,
        };

        let idx = priority as usize;
        match tx.try_send(req) {
            Ok(()) => {
                self.depth[idx].fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(req)) => {
                tracing::debug!(priority = ?priority, "router queue full — back-pressure");
                let _ = req.response_tx.send(None);
            }
            Err(mpsc::error::TrySendError::Closed(req)) => {
                let _ = req.response_tx.send(None);
            }
        }

        response_rx
    }

    /// Current queue depth for a priority tier (for metrics).
    pub fn depth(&self, priority: RequestPriority) -> usize {
        self.depth[priority as usize].load(Ordering::Relaxed)
    }
}

/// Receiver halves passed to worker tasks.
pub struct QueueReceivers {
    pub voice_rx: mpsc::Receiver<ClassificationRequest>,
    pub interactive_rx: mpsc::Receiver<ClassificationRequest>,
    pub background_rx: mpsc::Receiver<ClassificationRequest>,
}

/// Spawn `n` worker tasks that consume from the queue and classify via the LLM.
pub fn spawn_workers(
    n: usize,
    mut receivers: QueueReceivers,
    client: LlmRouterClient,
    depth: Arc<[AtomicUsize; 3]>,
) -> Vec<tokio::task::JoinHandle<()>> {
    // For n=1 we consume directly.  For n>1 we need to share the receivers,
    // but mpsc::Receiver is not Clone.  Use a simple approach: wrap in Arc<Mutex>
    // so workers can take turns pulling from the channels.
    //
    // With n=2 (default), contention is negligible since classification takes
    // 100-200ms and the mutex is held only for the recv() call.
    if n <= 1 {
        let client = client.clone();
        let depth = depth.clone();
        let handle = tokio::spawn(async move {
            worker_loop(
                &mut receivers.voice_rx,
                &mut receivers.interactive_rx,
                &mut receivers.background_rx,
                &client,
                &depth,
            )
            .await;
        });
        return vec![handle];
    }

    // For n > 1: wrap receivers in Arc<Mutex>.
    let voice_rx = Arc::new(tokio::sync::Mutex::new(receivers.voice_rx));
    let interactive_rx = Arc::new(tokio::sync::Mutex::new(receivers.interactive_rx));
    let background_rx = Arc::new(tokio::sync::Mutex::new(receivers.background_rx));

    (0..n)
        .map(|_| {
            let client = client.clone();
            let depth = depth.clone();
            let v = voice_rx.clone();
            let i = interactive_rx.clone();
            let b = background_rx.clone();
            tokio::spawn(async move {
                shared_worker_loop(v, i, b, &client, &depth).await;
            })
        })
        .collect()
}

async fn worker_loop(
    voice_rx: &mut mpsc::Receiver<ClassificationRequest>,
    interactive_rx: &mut mpsc::Receiver<ClassificationRequest>,
    background_rx: &mut mpsc::Receiver<ClassificationRequest>,
    client: &LlmRouterClient,
    depth: &[AtomicUsize; 3],
) {
    loop {
        let req = tokio::select! {
            biased;
            Some(req) = voice_rx.recv() => { depth[2].fetch_sub(1, Ordering::Relaxed); req }
            Some(req) = interactive_rx.recv() => { depth[1].fetch_sub(1, Ordering::Relaxed); req }
            Some(req) = background_rx.recv() => { depth[0].fetch_sub(1, Ordering::Relaxed); req }
            else => break,
        };
        process_request(req, client).await;
    }
}

async fn shared_worker_loop(
    voice_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<ClassificationRequest>>>,
    interactive_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<ClassificationRequest>>>,
    background_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<ClassificationRequest>>>,
    client: &LlmRouterClient,
    depth: &[AtomicUsize; 3],
) {
    loop {
        // Try voice first (biased priority), then interactive, then background.
        // Each lock is held only for the duration of try_recv().
        let req = if let Ok(r) = voice_rx.lock().await.try_recv() {
            depth[2].fetch_sub(1, Ordering::Relaxed);
            Some(r)
        } else if let Ok(r) = interactive_rx.lock().await.try_recv() {
            depth[1].fetch_sub(1, Ordering::Relaxed);
            Some(r)
        } else if let Ok(r) = background_rx.lock().await.try_recv() {
            depth[0].fetch_sub(1, Ordering::Relaxed);
            Some(r)
        } else {
            None
        };

        match req {
            Some(req) => process_request(req, client).await,
            None => {
                // No requests in any channel — yield briefly to avoid busy-spinning.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }
}

async fn process_request(req: ClassificationRequest, client: &LlmRouterClient) {
    let _wait_ms = req.enqueued_at.elapsed().as_millis();
    let result = client.classify(&req.message, req.sdr_suggestion.as_ref()).await;
    let _ = req.response_tx.send(result);
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn back_pressure_returns_none() {
        let (queue, _receivers) = RequestQueue::new(1);

        // Fill the channel (capacity 1).
        let _rx1 = queue.submit("msg1".into(), None, None, RequestPriority::Interactive);

        // Second submit should hit back-pressure.
        let rx2 = queue.submit("msg2".into(), None, None, RequestPriority::Interactive);

        // The back-pressured request gets None immediately.
        let result = rx2.await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn depth_tracks_enqueue() {
        let (queue, _receivers) = RequestQueue::new(8);
        assert_eq!(queue.depth(RequestPriority::Voice), 0);

        let _rx = queue.submit("test".into(), None, None, RequestPriority::Voice);
        assert_eq!(queue.depth(RequestPriority::Voice), 1);
    }
}
