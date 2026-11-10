use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::watch;

/// Completion of one observed engine run, not a durable save or child-task drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineRunCompletion {
    NormalReturn,
    AbnormalTermination,
    /// The handle was constructed without an observed background run.
    NotObserved,
}

pub(super) struct ObservedRun<F> {
    future: Option<Pin<Box<F>>>,
    completion: Option<watch::Sender<Option<EngineRunCompletion>>>,
}

pub(super) fn observe<F: Future<Output = ()>>(
    future: Pin<Box<F>>,
) -> (ObservedRun<F>, watch::Receiver<Option<EngineRunCompletion>>) {
    let (completion, receiver) = watch::channel(None);
    (
        ObservedRun {
            future: Some(future),
            completion: Some(completion),
        },
        receiver,
    )
}

impl<F> ObservedRun<F> {
    fn finish(&mut self, outcome: EngineRunCompletion) {
        drop(self.future.take());
        if let Some(completion) = self.completion.take() {
            let _ = completion.send_replace(Some(outcome));
        }
    }
}

impl<F: Future<Output = ()>> Future for ObservedRun<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        match this
            .future
            .as_mut()
            .expect("run already completed")
            .as_mut()
            .poll(cx)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => {
                this.finish(EngineRunCompletion::NormalReturn);
                Poll::Ready(())
            }
        }
    }
}

impl<F> Drop for ObservedRun<F> {
    fn drop(&mut self) {
        self.finish(EngineRunCompletion::AbnormalTermination);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::mock_engine_handle;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::oneshot;

    struct Run {
        release: oneshot::Receiver<()>,
        dropped: Arc<AtomicBool>,
        panic: bool,
        completion: Option<watch::Receiver<Option<EngineRunCompletion>>>,
    }

    impl Future for Run {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            assert!(!self.panic, "controlled run panic");
            Pin::new(&mut self.release).poll(cx).map(|_| ())
        }
    }

    impl Drop for Run {
        fn drop(&mut self) {
            if let Some(completion) = &self.completion {
                assert!(
                    completion.borrow().is_none(),
                    "completion published before run drop"
                );
            }
            self.dropped.store(true, Ordering::Release);
        }
    }

    fn fixture(
        panic: bool,
    ) -> (
        ObservedRun<Run>,
        crate::core::engine::EngineHandle,
        oneshot::Sender<()>,
        Arc<AtomicBool>,
    ) {
        let (release, receiver) = oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let (mut run, completion) = observe(Box::pin(Run {
            release: receiver,
            dropped: Arc::clone(&dropped),
            panic,
            completion: None,
        }));
        run.future.as_mut().unwrap().as_mut().get_mut().completion = Some(completion.clone());
        let mut handle = mock_engine_handle().handle;
        handle.run_completion = Some(completion);
        (run, handle, release, dropped)
    }

    #[tokio::test]
    async fn run_completion_drops_future_before_notifying_all_waiters() {
        let (run, handle, release, dropped) = fixture(false);
        let task = tokio::spawn(run);
        let mut cancelled = Box::pin(handle.wait_run_completion());
        std::future::poll_fn(|cx| {
            assert!(cancelled.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(cancelled);
        release.send(()).unwrap();
        let clone = handle.clone();
        let (first, second) =
            tokio::join!(handle.wait_run_completion(), clone.wait_run_completion());
        assert_eq!(first, EngineRunCompletion::NormalReturn);
        assert_eq!(second, first);
        assert!(dropped.load(Ordering::Acquire));
        task.await.unwrap();
        assert_eq!(handle.wait_run_completion().await, first);
    }

    #[test]
    fn run_completion_supervised_panic_is_abnormal_after_future_drop() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (run, handle, _release, dropped) = fixture(true);
            let task = codewhale_util::utils::spawn_supervised(
                "completion-test",
                std::panic::Location::caller(),
                run,
            );
            task.await.expect("existing supervisor catches the panic");
            assert_eq!(
                handle.wait_run_completion().await,
                EngineRunCompletion::AbnormalTermination
            );
            assert!(dropped.load(Ordering::Acquire));
        });
    }

    #[tokio::test]
    async fn run_completion_unpolled_abort_drops_future_before_notification() {
        let (run, handle, _release, dropped) = fixture(false);
        let task = tokio::spawn(run);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            handle.wait_run_completion().await,
            EngineRunCompletion::AbnormalTermination
        );
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn run_completion_polled_abort_drops_future_before_notification() {
        let (mut run, handle, _release, dropped) = fixture(false);
        std::future::poll_fn(|cx| {
            assert!(Pin::new(&mut run).poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let task = tokio::spawn(run);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            handle.wait_run_completion().await,
            EngineRunCompletion::AbnormalTermination
        );
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn run_completion_runtime_drop_is_abnormal() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (run, handle, _release, dropped) = fixture(false);
        runtime.spawn(run);
        drop(runtime);
        assert!(dropped.load(Ordering::Acquire));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            runtime.block_on(handle.wait_run_completion()),
            EngineRunCompletion::AbnormalTermination
        );
    }

    #[tokio::test]
    async fn run_completion_old_run_cannot_complete_another_run() {
        let (old, old_handle, release, _) = fixture(false);
        let (_new, new_handle, _new_release, _) = fixture(false);
        release.send(()).unwrap();
        old.await;
        assert_eq!(
            old_handle.wait_run_completion().await,
            EngineRunCompletion::NormalReturn
        );
        let mut pending = Box::pin(new_handle.wait_run_completion());
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
}
