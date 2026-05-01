//! Synchronous bridge for child-worker operations.
//!
//! Assembly and topology execution are still exposed through synchronous trait
//! boundaries. This helper runs their async worker futures without panicking
//! when the caller happens to be inside a current-thread Tokio runtime.

use std::future::Future;

use tokio::runtime::{Builder, Handle, RuntimeFlavor};

pub(crate) fn run_worker_future<T, Fut, MakeFuture>(
    make_future: MakeFuture,
    runtime_label: &str,
) -> T
where
    T: Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    MakeFuture: FnOnce() -> Fut + Send + 'static,
{
    let runtime_label = runtime_label.to_string();
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(move || handle.block_on(make_future()))
        }
        Ok(_) => std::thread::spawn({
            let runtime_label_for_thread = runtime_label.clone();
            move || {
                Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap_or_else(|_| {
                        panic!("{runtime_label_for_thread} should build a current-thread runtime")
                    })
                    .block_on(make_future())
            }
        })
        .join()
        .unwrap_or_else(|_| panic!("{runtime_label} worker thread panicked")),
        Err(_) => Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|_| panic!("{runtime_label} should build a current-thread runtime"))
            .block_on(make_future()),
    }
}
