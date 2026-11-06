//! Data-pipeline helpers — port of `utils/pl` (Go channels + semaphore;
//! here bounded worker threads with a shared cancel token).

use crate::context::Cancellation;
use std::sync::{Arc, Mutex};

/// Processes `input` through `stage_fn` with at most `max_workers` concurrent
/// invocations, returning successful outputs and errors in completion order
/// (`pl.Stage`).
///
/// Unlike Go's channel pair, cancellation is cooperative: the returned
/// [`StageHandle`] exposes `output`/`errors` and a `cancel` token; workers stop
/// pulling new work once it is cancelled.
pub struct StageHandle<Out, E> {
    /// Successful stage outputs, in completion order.
    pub output: Mutex<Vec<Out>>,
    /// Stage errors, in completion order.
    pub errors: Mutex<Vec<E>>,
    /// Cooperative cancellation token for the stage.
    pub cancel: Cancellation,
    done: Mutex<bool>,
}

impl<Out, E> StageHandle<Out, E> {
    /// Blocks until all workers have drained the input.
    pub fn wait(&self) {
        while !*self.done.lock().expect("done lock") {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// Runs `stage_fn` over each element of `input`, bounded by `max_workers`
/// concurrent invocations (`pl.Stage`). `tokio`'s semaphore semantics are
/// replaced by a thread pool sized `max_workers` pulling from a shared queue.
pub fn stage<In, Out, E>(
    max_workers: usize,
    input: Vec<In>,
    stage_fn: impl Fn(In) -> Result<Out, E> + Send + Sync + 'static,
) -> Arc<StageHandle<Out, E>>
where
    In: Send + 'static,
    Out: Send + 'static,
    E: Send + 'static,
{
    let handle = Arc::new(StageHandle::<Out, E> {
        output: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
        cancel: Cancellation::new(),
        done: Mutex::new(false),
    });
    let queue = Arc::new(Mutex::new(
        input
            .into_iter()
            .collect::<std::collections::VecDeque<In>>(),
    ));

    std::thread::scope(|s| {
        let mut threads = Vec::new();
        let workers = max_workers.max(1);
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let handle = Arc::clone(&handle);
            let stage_fn = &stage_fn;
            threads.push(s.spawn(move || loop {
                if handle.cancel.is_cancelled() {
                    return;
                }
                let item = queue.lock().expect("queue lock").pop_front();
                let Some(item) = item else {
                    return;
                };
                match stage_fn(item) {
                    Ok(out) => handle.output.lock().expect("out lock").push(out),
                    Err(e) => handle.errors.lock().expect("err lock").push(e),
                }
            }));
        }
        for t in threads {
            let _ = t.join();
        }
        *handle.done.lock().expect("done lock") = true;
    });
    handle
}

/// Runs `sink_fn` over each element of `input`, bounded by `max_workers`
/// concurrent invocations, discarding output and collecting errors
/// (`pl.Sink`).
pub fn sink<In, E>(
    max_workers: usize,
    input: Vec<In>,
    sink_fn: impl Fn(In) -> Result<(), E> + Send + Sync + 'static,
) -> Vec<E>
where
    In: Send + 'static,
    E: Clone + Send + 'static,
{
    let handle: Arc<StageHandle<(), E>> = stage(max_workers, input, sink_fn);
    let errors = handle.errors.lock().expect("err lock").clone();
    errors
}

/// Merges the outputs of multiple iterators into one interleaved sequence.
/// Unlike Go's channel merge, this is a synchronous round-robin zip
/// (`pl.Merge`); async merging belongs to the stream crate.
pub fn merge<T: Clone>(inputs: &[Vec<T>]) -> Vec<T> {
    let max_len = inputs.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::with_capacity(inputs.iter().map(Vec::len).sum());
    for i in 0..max_len {
        for input in inputs {
            if let Some(v) = input.get(i) {
                out.push(v.clone());
            }
        }
    }
    out
}

/// Splits `input` into two independent vectors (`pl.Tee`).
#[must_use]
pub fn tee<T: Clone>(input: Vec<T>) -> (Vec<T>, Vec<T>) {
    (input.clone(), input)
}

/// Collects a slice into a vector consumed element-by-element
/// (`pl.FromSlice`); a no-op in Rust, kept for call-site parity.
#[must_use]
pub fn from_slice<T: Clone>(input: &[T]) -> Vec<T> {
    input.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_calls_the_transform_and_returns_values_and_errors() {
        let input: Vec<i32> = vec![0, 1, 2, 3];
        let handle = stage(1, input, |i| {
            if i % 2 == 0 {
                Err(format!("even number {i}"))
            } else {
                Ok(i * 2)
            }
        });
        handle.wait();
        assert_eq!(handle.output.lock().expect("out").clone(), vec![2, 6]);
        let errs = handle.errors.lock().expect("err").clone();
        assert_eq!(errs, vec!["even number 0", "even number 2"]);
    }

    #[test]
    fn stage_respects_the_worker_limit() {
        const MAX_WORKERS: usize = 2;
        const NUM_JOBS: usize = 100;
        let input: Vec<usize> = (0..NUM_JOBS).collect();
        let current = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (current2, max2, count2) = (
            std::sync::Arc::clone(&current),
            std::sync::Arc::clone(&max),
            std::sync::Arc::clone(&count),
        );
        let handle = stage(MAX_WORKERS, input, move |_| {
            let current = &current2;
            let max = &max2;
            let count = &count2;
            let c = current.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            max.fetch_max(c, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(2));
            current.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<(), String>(())
        });
        handle.wait();
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), NUM_JOBS);
        assert!(max.load(std::sync::atomic::Ordering::SeqCst) <= MAX_WORKERS);
    }

    #[test]
    fn sink_collects_errors_and_discards_output() {
        let input: Vec<i32> = vec![0, 1, 2];
        let errs = sink(2, input, |i| {
            if i % 2 == 0 {
                Err(format!("even {i}"))
            } else {
                Ok(())
            }
        });
        assert_eq!(errs.len(), 2);
    }

    #[test]
    fn merge_interleaves_inputs() {
        let merged = merge(&[vec![1, 2], vec![10, 20, 30]]);
        assert_eq!(merged, vec![1, 10, 2, 20, 30]);
        assert!(merge::<i32>(&[]).is_empty());
    }

    #[test]
    fn tee_duplicates_the_input() {
        let (a, b) = tee(vec![1, 2, 3]);
        assert_eq!(a, [1, 2, 3]);
        assert_eq!(b, [1, 2, 3]);
    }

    #[test]
    fn from_slice_copies_the_input() {
        assert_eq!(from_slice(&[1, 2]), [1, 2]);
    }
}
