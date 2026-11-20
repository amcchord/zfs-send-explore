//! Cooperative progress and cancellation for synchronous desktop workers.
//! A control is scoped to one worker's call stack. CLI callers need no control.
use serde::Serialize;
use std::cell::RefCell;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Progress {
    pub read_bytes: u64,
    pub written_bytes: u64,
    pub files: u64,
}

#[derive(Clone, Default)]
pub struct JobControl(Arc<AtomicBool>);

struct Observer {
    control: JobControl,
    progress: Progress,
    last_report: Option<Instant>,
    report: Box<dyn Fn(Progress)>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Observer>> = const { RefCell::new(None) };
}

impl JobControl {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn run<T>(&self, report: impl Fn(Progress) + 'static, operation: impl FnOnce() -> T) -> T {
        struct Restore(Option<Observer>);
        impl Drop for Restore {
            fn drop(&mut self) {
                ACTIVE.set(self.0.take());
            }
        }
        let _scope = Restore(ACTIVE.replace(Some(Observer {
            control: self.clone(),
            progress: Progress::default(),
            last_report: None,
            report: Box::new(report),
        })));
        let result = operation();
        ACTIVE.with_borrow(|job| {
            if let Some(observer) = job {
                (observer.report)(observer.progress);
            }
        });
        result
    }
}

fn advance(read: u64, written: u64) -> io::Result<()> {
    ACTIVE.with_borrow_mut(|active| {
        if let Some(job) = active {
            if job.control.is_cancelled() {
                return Err(io::Error::other("Operation cancelled"));
            }
            job.progress.read_bytes = job.progress.read_bytes.saturating_add(read);
            job.progress.written_bytes = job.progress.written_bytes.saturating_add(written);
            if job
                .last_report
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(180))
            {
                (job.report)(job.progress);
                job.last_report = Some(Instant::now());
            }
        }
        Ok(())
    })
}

pub fn checkpoint() -> io::Result<()> {
    advance(0, 0)
}
pub(crate) fn read(bytes: u64) -> io::Result<()> {
    advance(bytes, 0)
}
pub(crate) fn written(bytes: u64) -> io::Result<()> {
    advance(0, bytes)
}
// Once a file has been published, reporting it must never turn success into cancellation.
pub(crate) fn completed_file() {
    ACTIVE.with_borrow_mut(|job| {
        if let Some(observer) = job {
            observer.progress.files += 1;
        }
    });
}
