//! 固定大小的工作线程池。
//!
//! 每个 worker 在自己的线程里建一套 `Registry`（自己的 Lua 状态），只接收闭包，
//! 返回结果。主线程负责 store、快照和提交；Lua 指针永远不跨线程。

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::registry::{Definitions, Registry};

type Job = Box<dyn FnOnce(&Registry) + Send + 'static>;

/// 线程池错误。
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// worker 初始化失败。
    #[error("worker init: {0}")]
    Init(String),
    /// worker 不在了。
    #[error("worker is gone")]
    Gone,
    /// worker 内的闭包 panic 了。
    #[error("worker panicked")]
    Panicked,
}

enum Pending<R> {
    Waiting(Receiver<thread::Result<R>>),
    Failed(PoolError),
}

/// 一组线程亲和的解释器。
pub struct Pool {
    senders: Vec<Sender<Job>>,
    handles: Vec<JoinHandle<()>>,
    next: AtomicUsize,
    size: usize,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool").field("size", &self.size).finish()
    }
}

impl Pool {
    /// 每个 worker 用共享的 `Definitions` 建自己的解释器。
    pub fn new(defs: Arc<Definitions>, size: usize) -> Result<Self, PoolError> {
        let size = size.max(1);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let mut senders = Vec::with_capacity(size);
        let mut handles = Vec::with_capacity(size);
        for index in 0..size {
            let (tx, rx) = mpsc::channel::<Job>();
            let defs = defs.clone();
            let ready = ready_tx.clone();
            let handle = thread::Builder::new()
                .name(format!("synap-worker-{index}"))
                .spawn(move || match Registry::from_shared(defs) {
                    Ok(registry) => {
                        let _ = ready.send(Ok(()));
                        while let Ok(job) = rx.recv() {
                            job(&registry);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                    }
                })
                .map_err(|error| PoolError::Init(error.to_string()))?;
            senders.push(tx);
            handles.push(handle);
        }
        drop(ready_tx);
        for _ in 0..size {
            match ready_rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(PoolError::Init(error)),
                Err(_) => return Err(PoolError::Init("worker exited during init".to_owned())),
            }
        }
        Ok(Self {
            senders,
            handles,
            next: AtomicUsize::new(0),
            size,
        })
    }

    /// worker 数量。
    pub fn size(&self) -> usize {
        self.size
    }

    /// 分发一批闭包，按提交顺序返回结果。闭包在不同 worker 上并行执行。
    pub fn run_many<R, F>(&self, jobs: Vec<F>) -> Vec<Result<R, PoolError>>
    where
        F: FnOnce(&Registry) -> R + Send + 'static,
        R: Send + 'static,
    {
        let mut pending = Vec::with_capacity(jobs.len());
        for job in jobs {
            let (tx, rx) = mpsc::channel::<thread::Result<R>>();
            let wrapped: Job = Box::new(move |registry| {
                let result = catch_unwind(AssertUnwindSafe(|| job(registry)));
                let _ = tx.send(result);
            });
            let index = self.next.fetch_add(1, Ordering::Relaxed) % self.size;
            match self.senders[index].send(wrapped) {
                Ok(()) => pending.push(Pending::Waiting(rx)),
                Err(_) => pending.push(Pending::Failed(PoolError::Gone)),
            }
        }
        pending
            .into_iter()
            .map(|item| match item {
                Pending::Waiting(rx) => match rx.recv() {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(_)) => Err(PoolError::Panicked),
                    Err(_) => Err(PoolError::Gone),
                },
                Pending::Failed(error) => Err(error),
            })
            .collect()
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.senders.clear();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}
