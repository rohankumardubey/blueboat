//! V8 isolate owner threads and pools.

use rusty_v8 as v8;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Semaphore};

/// JavaScript-side runtime.
static LIBRT: &'static str = include_str!("../../librt/dist/main.js");

pub struct IsolateThreadPool {
    /// Manage the pool as a stack to optimize for cache.
    threads: std::sync::Mutex<Vec<IsolateThread>>,

    /// Pool acq/rel notifier.
    notifier: Semaphore,
}

/// A handle to a thread that owns an v8::Isolate.
struct IsolateThread {
    job_tx: mpsc::Sender<IsolateJob>,
}

#[derive(Debug, Clone)]
pub struct IsolateConfig {
    pub max_memory_bytes: usize,
}

struct ThreadGuard<'a> {
    pool: &'a IsolateThreadPool,
    th: Option<IsolateThread>,
}

impl<'a> Drop for ThreadGuard<'a> {
    fn drop(&mut self) {
        self.pool
            .threads
            .lock()
            .unwrap()
            .push(self.th.take().unwrap());
    }
}

impl<'a> std::ops::Deref for ThreadGuard<'a> {
    type Target = IsolateThread;
    fn deref(&self) -> &Self::Target {
        self.th.as_ref().unwrap()
    }
}

pub type IsolateJob = Box<dyn FnOnce(&mut v8::ContextScope<'_, v8::HandleScope<'_>>) + Send>;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IsolateGeneration(pub u64);

#[derive(Clone)]
pub struct IsolateGenerationBox(pub Arc<std::sync::Mutex<IsolateGeneration>>);

impl IsolateThreadPool {
    pub async fn new(size: usize, config: IsolateConfig) -> Self {
        let start_time = std::time::Instant::now();
        let threads: Vec<IsolateThread> =
            futures::future::join_all((0..size).map(|_| IsolateThread::new(config.clone()))).await;
        let end_time = std::time::Instant::now();
        info!(
            "isolate pool of size {} initialized in {:?}",
            size,
            end_time.duration_since(start_time)
        );
        Self {
            threads: std::sync::Mutex::new(threads),
            notifier: Semaphore::new(size),
        }
    }

    pub async fn run<
        R: Send + 'static,
        F: FnOnce(&mut v8::ContextScope<'_, v8::HandleScope<'_>>) -> R + Send + 'static,
    >(
        &self,
        job: F,
    ) -> R {
        let _permit = self.notifier.acquire().await;
        let th = self
            .threads
            .lock()
            .unwrap()
            .pop()
            .expect("IsolateThreadPool::run: got permit but no thread available");

        // Return the thread back to the pool in case of async cancellation.
        // Drop order ensure that `permit` is released after `guard`.
        let guard = ThreadGuard {
            pool: self,
            th: Some(th),
        };

        let (ret_tx, ret_rx) = oneshot::channel();
        guard
            .job_tx
            .send(Box::new(|scope| {
                drop(ret_tx.send(job(scope)));
            }))
            .await
            .map_err(|_| "cannot send to job_tx")
            .unwrap();
        ret_rx.await.unwrap()
    }
}

impl IsolateThread {
    pub async fn new(config: IsolateConfig) -> Self {
        let (job_tx, job_rx) = mpsc::channel(1);
        let (init_tx, init_rx) = oneshot::channel();
        std::thread::spawn(|| isolate_worker(config, init_tx, job_rx));
        init_rx
            .await
            .expect("IsolateThread::new: isolate_worker did not send a response");
        Self { job_tx }
    }
}

fn isolate_worker(
    config: IsolateConfig,
    init_tx: oneshot::Sender<()>,
    mut job_rx: mpsc::Receiver<IsolateJob>,
) {
    let params = v8::Isolate::create_params().heap_limits(0, config.max_memory_bytes);

    // Must not be moved
    let mut isolate = v8::Isolate::new(params);

    let librt_persistent;

    // Compile librt.
    // Many unwraps here! but since we are initializing it should be fine.
    {
        let mut isolate_scope = v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(&mut isolate_scope);
        let mut context_scope = v8::ContextScope::new(&mut isolate_scope, context);
        let scope = &mut v8::HandleScope::new(&mut context_scope);

        let librt = v8::String::new(scope, LIBRT).unwrap();
        let librt = v8::Script::compile(scope, librt, None)
            .unwrap()
            .get_unbound_script(scope);
        librt_persistent = v8::Global::new(scope, librt);
    }

    // May fail if the receiver side is cancelled
    drop(init_tx.send(()));

    let generation = IsolateGenerationBox(Arc::new(std::sync::Mutex::new(IsolateGeneration(0))));
    isolate.set_slot(generation.clone());

    loop {
        let job = match job_rx.blocking_recv() {
            Some(x) => x,
            None => break,
        };

        // Enter context.
        let mut isolate_scope = v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(&mut isolate_scope);
        let mut context_scope = v8::ContextScope::new(&mut isolate_scope, context);

        // Run librt initialization.
        {
            let scope = &mut v8::HandleScope::new(&mut context_scope);

            let global_key = v8::String::new(scope, "global").unwrap();
            let global_obj = scope.get_current_context().global(scope);
            global_obj.set(scope, global_key.into(), global_obj.into());

            let librt = v8::Local::<'_, v8::UnboundScript>::new(scope, librt_persistent.clone())
                .bind_to_current_context(scope);
            librt.run(scope);
        }

        job(&mut context_scope);

        // Cleanup instance state so that we can reuse it.
        // Keep in sync with InstanceHandle::do_remote_termination.

        // Acquire generation lock.
        let mut generation = generation.0.lock().unwrap();

        // Advance generation.
        generation.0 += 1;

        // Cleanup termination.
        context_scope.cancel_terminate_execution();

        // Drop the lock.
        drop(generation);

        // Cleanup slots.
        crate::executor::Instance::cleanup(&mut context_scope);

        // Reset memory limit.
        context_scope.remove_near_heap_limit_callback(
            crate::executor::on_memory_limit_exceeded,
            config.max_memory_bytes,
        );
    }
}