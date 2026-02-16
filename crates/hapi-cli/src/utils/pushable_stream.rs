use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Error;
use futures::Stream;
use tokio::sync::mpsc;

/// Sender half of a pushable async stream.
#[derive(Debug)]
pub struct PushableSender<T> {
    tx: mpsc::Sender<Result<T, Error>>,
}

impl<T: Send + 'static> PushableSender<T> {
    /// Push a value into the stream.
    pub async fn push(&self, value: T) -> Result<(), Error> {
        self.tx
            .send(Ok(value))
            .await
            .map_err(|_| anyhow::anyhow!("receiver dropped"))
    }

    /// Signal the end of the stream by dropping the sender.
    pub fn end(self) {
        // Dropping self closes the channel.
    }

    /// Push an error into the stream and close it.
    pub async fn set_error(&self, err: Error) {
        let _ = self.tx.send(Err(err)).await;
    }
}

impl<T> Clone for PushableSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

/// Receiver half of a pushable async stream.
/// Implements `Stream<Item = Result<T, Error>>`.
pub struct PushableReceiver<T> {
    rx: mpsc::Receiver<Result<T, Error>>,
}

impl<T> Stream for PushableReceiver<T> {
    type Item = Result<T, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Create a new pushable stream pair with the given buffer size.
pub fn pushable_stream<T: Send + 'static>(
    buffer_size: usize,
) -> (PushableSender<T>, PushableReceiver<T>) {
    let (tx, rx) = mpsc::channel(buffer_size);
    (PushableSender { tx }, PushableReceiver { rx })
}
