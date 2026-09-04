use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use common::sync::recover_lock;

pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct WorkerPool {
    sender: Option<mpsc::Sender<Job>>,
    workers: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    pub(crate) fn new(size: usize, dispatch: tracing::Dispatch) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(size);
        for id in 0..size {
            let receiver = Arc::clone(&receiver);
            let dispatch = dispatch.clone();
            let handle = thread::Builder::new()
                .name(format!("engine-worker-{id}"))
                .spawn(move || worker_loop(&receiver, &dispatch))?;
            workers.push(handle);
        }
        Ok(Self { sender: Some(sender), workers })
    }

    pub(crate) fn submit(&self, job: Job) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(job);
        }
    }
}

fn worker_loop(receiver: &Arc<Mutex<mpsc::Receiver<Job>>>, dispatch: &tracing::Dispatch) {
    tracing::dispatcher::with_default(dispatch, || {
        loop {
            let job = {
                let receiver = recover_lock(receiver.lock(), "WorkerPool.receiver");
                receiver.recv()
            };
            match job {
                Ok(job) => job(),
                Err(_) => break,
            }
        }
    });
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
