use std::collections::VecDeque;
use std::time::{Duration, Instant};

const MAX_BYTES: usize = 16 * 1024 * 1024; // 16MB
const MAX_ITEMS: usize = 500;
const MAX_ITEM_BYTES: usize = 1024 * 1024; // 1MB
const MAX_AGE: Duration = Duration::from_secs(15 * 60); // 15 minutes

struct QueuedItem {
    data: String,
    enqueued_at: Instant,
}

pub struct SocketOutbox {
    queue: VecDeque<QueuedItem>,
    total_bytes: usize,
}

impl SocketOutbox {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            total_bytes: 0,
        }
    }

    pub fn enqueue(&mut self, _event: &str, data: &str) {
        if data.len() > MAX_ITEM_BYTES {
            tracing::warn!(size = data.len(), "outbox item too large, dropping");
            return;
        }

        // Remove expired items
        self.prune_expired();

        // Make room if needed
        while self.queue.len() >= MAX_ITEMS || self.total_bytes + data.len() > MAX_BYTES {
            if let Some(removed) = self.queue.pop_front() {
                self.total_bytes -= removed.data.len();
            } else {
                break;
            }
        }

        self.total_bytes += data.len();
        self.queue.push_back(QueuedItem {
            data: data.to_string(),
            enqueued_at: Instant::now(),
        });
    }

    pub fn drain(&mut self) -> Vec<String> {
        self.prune_expired();
        let items: Vec<String> = self.queue.drain(..).map(|i| i.data).collect();
        self.total_bytes = 0;
        items
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.queue.front() {
            if now.duration_since(front.enqueued_at) > MAX_AGE {
                if let Some(removed) = self.queue.pop_front() {
                    self.total_bytes -= removed.data.len();
                }
            } else {
                break;
            }
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}
