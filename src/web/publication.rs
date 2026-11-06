use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{Mutex as AsyncMutex, RwLock};

/// Independent target updates share a catalog read lease, but a single strategy
/// keeps its lease through authorization, save, archive and every account write.
/// Catalog/binding edits take the write lease so a stop cannot be followed by an
/// in-flight publish using its former shares. No strategy waiter holds a lease.
#[derive(Default)]
pub(super) struct PublicationLocks {
    strategies: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

impl PublicationLocks {
    /// Configuration writes close admission instead of making real-time requests
    /// wait behind a PostgreSQL operation on the configuration path.
    pub(super) async fn run_available<T: Send + 'static>(
        &self,
        configuration: Arc<RwLock<()>>,
        strategy: &str,
        work: impl Future<Output = T> + Send + 'static,
    ) -> Option<Result<T, tokio::task::JoinError>> {
        let lock = {
            let mut strategies = self.strategies.lock().unwrap_or_else(|e| e.into_inner());
            strategies.retain(|_, lock| lock.strong_count() > 0);
            strategies
                .entry(strategy.to_owned())
                .or_default()
                .upgrade()
                .unwrap_or_else(|| {
                    let lock = Arc::new(AsyncMutex::new(()));
                    strategies.insert(strategy.to_owned(), Arc::downgrade(&lock));
                    lock
                })
        };
        let strategy_guard = lock.lock_owned().await;
        let configuration_guard = configuration.try_read_owned().ok()?;
        Some(
            tokio::spawn(async move {
                let _strategy_guard = strategy_guard;
                let _configuration_guard = configuration_guard;
                work.await
            })
            .await,
        )
    }

    #[cfg(test)]
    pub(super) async fn run<T: Send + 'static>(
        &self,
        configuration: Arc<RwLock<()>>,
        strategy: &str,
        work: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, tokio::task::JoinError> {
        let lock = {
            let mut strategies = self.strategies.lock().unwrap_or_else(|e| e.into_inner());
            // Do not retain arbitrary strategy names from rejected requests.
            strategies.retain(|_, lock| lock.strong_count() > 0);
            match strategies.get(strategy).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(AsyncMutex::new(()));
                    strategies.insert(strategy.to_owned(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        let strategy_guard = lock.lock_owned().await;
        let configuration_guard = configuration.read_owned().await;
        // Waiting HTTP requests remain cancellable. Once admitted, keep the
        // complete publish and both guards alive even if the client disconnects.
        // This is process-local completion, not a durable queue across restarts.
        tokio::spawn(async move {
            let _strategy_guard = strategy_guard;
            let _configuration_guard = configuration_guard;
            work.await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::Poll;
    use tokio::sync::oneshot;
    use tokio::time::{Duration, timeout};

    async fn pending<T>(mut future: Pin<&mut impl Future<Output = T>>) {
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
                .await
                .is_pending()
        );
    }

    async fn finish<T>(future: impl Future<Output = T>) -> T {
        timeout(Duration::from_secs(5), future).await.unwrap()
    }

    #[tokio::test]
    async fn slow_strategy_does_not_block_another_strategy() {
        let locks = PublicationLocks::default();
        let configuration = Arc::new(RwLock::new(()));
        let (release, wait) = oneshot::channel();
        let (started, entered) = oneshot::channel();
        let mut slow = Box::pin(locks.run(configuration.clone(), "slow", async move {
            started.send(()).unwrap();
            wait.await.unwrap();
            1
        }));
        pending(slow.as_mut()).await;
        finish(entered).await.unwrap();
        assert_eq!(
            finish(locks.run(configuration, "fast", async { 2 }))
                .await
                .unwrap(),
            2
        );
        release.send(()).unwrap();
        assert_eq!(finish(slow).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn same_strategy_updates_keep_order_through_all_account_writes() {
        let locks = PublicationLocks::default();
        let configuration = Arc::new(RwLock::new(()));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let first_writes = writes.clone();
        let (release, wait) = oneshot::channel();
        let (started, entered) = oneshot::channel();
        let mut first = Box::pin(locks.run(configuration.clone(), "same", async move {
            first_writes.lock().unwrap().push((1, "account-a"));
            started.send(()).unwrap();
            wait.await.unwrap();
            first_writes.lock().unwrap().push((1, "account-b"));
        }));
        pending(first.as_mut()).await;
        finish(entered).await.unwrap();
        let second_writes = writes.clone();
        let mut second = Box::pin(locks.run(configuration, "same", async move {
            second_writes
                .lock()
                .unwrap()
                .extend([(2, "account-a"), (2, "account-b")]);
        }));
        pending(second.as_mut()).await;
        release.send(()).unwrap();
        finish(first).await.unwrap();
        finish(second).await.unwrap();
        assert_eq!(
            *writes.lock().unwrap(),
            [
                (1, "account-a"),
                (1, "account-b"),
                (2, "account-a"),
                (2, "account-b")
            ]
        );
    }

    #[tokio::test]
    async fn disconnect_keeps_admitted_publish_and_excludes_configuration_changes() {
        let locks = PublicationLocks::default();
        let configuration = Arc::new(RwLock::new(()));
        let (release, wait) = oneshot::channel();
        let (started, entered) = oneshot::channel();
        let (completed, completion) = oneshot::channel();
        let mut request = Box::pin(locks.run(configuration.clone(), "same", async move {
            started.send(()).unwrap();
            wait.await.unwrap();
            completed.send(()).unwrap();
        }));
        pending(request.as_mut()).await;
        finish(entered).await.unwrap();
        drop(request);
        let mut stop = Box::pin(configuration.write());
        pending(stop.as_mut()).await;
        let mut next = Box::pin(locks.run(configuration.clone(), "same", async {}));
        pending(next.as_mut()).await;
        release.send(()).unwrap();
        finish(completion).await.unwrap();
        let stop_guard = finish(stop).await;
        // Same-strategy waiters must not hold a read lease ahead of the stop.
        pending(next.as_mut()).await;
        drop(stop_guard);
        finish(next).await.unwrap();
    }

    #[tokio::test]
    async fn disconnected_waiter_never_publishes_later() {
        let locks = PublicationLocks::default();
        let configuration = Arc::new(RwLock::new(()));
        let editing = configuration.write().await;
        let (sent, received) = oneshot::channel();
        let mut waiting = Box::pin(locks.run(configuration.clone(), "waiting", async move {
            sent.send(()).unwrap();
        }));
        pending(waiting.as_mut()).await;
        drop(waiting);
        drop(editing);
        assert!(finish(received).await.is_err());
        finish(locks.run(configuration, "waiting", async {}))
            .await
            .unwrap();
        assert!(
            locks
                .strategies
                .lock()
                .unwrap()
                .values()
                .all(|entry| entry.strong_count() == 0)
        );
    }
}
