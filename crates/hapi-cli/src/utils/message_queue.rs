use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};

/// A single item in the queue, tagged with mode and mode hash.
#[derive(Debug, Clone)]
struct QueueItem<T> {
    message: String,
    mode: T,
    mode_hash: String,
    isolate: bool,
}

/// Result of collecting a batch of same-mode messages.
#[derive(Debug, Clone)]
pub struct BatchResult<T> {
    pub message: String,
    pub mode: T,
    pub hash: String,
    pub isolate: bool,
}

/// Shared inner state of the queue.
struct Inner<T> {
    queue: VecDeque<QueueItem<T>>,
    closed: bool,
    mode_hasher: Box<dyn Fn(&T) -> String + Send + Sync>,
}

/// A mode-aware message queue that stores messages with their modes.
/// Returns consistent batches of messages with the same mode.
pub struct MessageQueue2<T> {
    inner: Arc<Mutex<Inner<T>>>,
    notify: Arc<Notify>,
}

impl<T: Clone + Send + 'static> MessageQueue2<T> {
    pub fn new(mode_hasher: impl Fn(&T) -> String + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                queue: VecDeque::new(),
                closed: false,
                mode_hasher: Box::new(mode_hasher),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Push a message to the back of the queue.
    pub async fn push(&self, message: String, mode: T) {
        let mut inner = self.inner.lock().await;
        if inner.closed {
            return;
        }
        let mode_hash = (inner.mode_hasher)(&mode);
        inner.queue.push_back(QueueItem {
            message,
            mode,
            mode_hash,
            isolate: false,
        });
        drop(inner);
        self.notify.notify_one();
    }

    /// Push a message and wake the waiter immediately.
    pub async fn push_immediate(&self, message: String, mode: T) {
        let mut inner = self.inner.lock().await;
        if inner.closed {
            return;
        }
        let mode_hash = (inner.mode_hasher)(&mode);
        inner.queue.push_back(QueueItem {
            message,
            mode,
            mode_hash,
            isolate: false,
        });
        drop(inner);
        self.notify.notify_one();
    }

    /// Clear the queue, then push a single isolated message.
    pub async fn push_isolate_and_clear(&self, message: String, mode: T) {
        let mut inner = self.inner.lock().await;
        if inner.closed {
            return;
        }
        let mode_hash = (inner.mode_hasher)(&mode);
        inner.queue.clear();
        inner.queue.push_back(QueueItem {
            message,
            mode,
            mode_hash,
            isolate: true,
        });
        drop(inner);
        self.notify.notify_one();
    }

    /// Push a message to the front of the queue.
    pub async fn unshift(&self, message: String, mode: T) {
        let mut inner = self.inner.lock().await;
        if inner.closed {
            return;
        }
        let mode_hash = (inner.mode_hasher)(&mode);
        inner.queue.push_front(QueueItem {
            message,
            mode,
            mode_hash,
            isolate: false,
        });
        drop(inner);
        self.notify.notify_one();
    }

    /// Clear all messages and reopen the queue.
    pub async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        inner.queue.clear();
        inner.closed = false;
    }

    /// Close the queue. No more messages can be pushed.
    /// Wakes any waiting consumer.
    pub async fn close(&self) {
        let mut inner = self.inner.lock().await;
        inner.closed = true;
        drop(inner);
        self.notify.notify_one();
    }

    /// Current number of messages in the queue.
    pub async fn size(&self) -> usize {
        self.inner.lock().await.queue.len()
    }

    /// Whether the queue has been closed.
    pub async fn is_closed(&self) -> bool {
        self.inner.lock().await.closed
    }

    /// Wait for messages and return a batch of same-mode messages joined
    /// with newlines. Returns `None` if the queue is closed with no
    /// remaining messages.
    pub async fn wait_for_messages(&self) -> Option<BatchResult<T>> {
        loop {
            {
                let mut inner = self.inner.lock().await;
                if let Some(batch) = Self::collect_batch(&mut inner.queue) {
                    return Some(batch);
                }
                if inner.closed {
                    return None;
                }
            }
            // Wait until notified, then re-check.
            self.notify.notified().await;
        }
    }

    /// Collect a batch of consecutive same-mode-hash messages from the
    /// front of the queue. Isolated messages are returned alone.
    fn collect_batch(queue: &mut VecDeque<QueueItem<T>>) -> Option<BatchResult<T>> {
        let first = queue.front()?;
        let target_hash = first.mode_hash.clone();
        let mode = first.mode.clone();
        let isolate = first.isolate;

        if isolate {
            let item = queue.pop_front().unwrap();
            return Some(BatchResult {
                message: item.message,
                mode: item.mode,
                hash: target_hash,
                isolate: true,
            });
        }

        let mut messages = Vec::new();
        while let Some(front) = queue.front() {
            if front.mode_hash != target_hash || front.isolate {
                break;
            }
            messages.push(queue.pop_front().unwrap().message);
        }

        Some(BatchResult {
            message: messages.join("\n"),
            mode,
            hash: target_hash,
            isolate: false,
        })
    }
}