//! Concurrent error handling — port of `utils/run`.

/// Runs the given functions sequentially, stopping at the first error
/// (`run.Sequentially`).
///
/// # Errors
/// The first error returned by any `f`, if any.
pub fn sequentially<E>(fs: &[&dyn Fn() -> Result<(), E>]) -> Result<(), E> {
    for f in fs {
        f()?;
    }
    Ok(())
}

/// Runs the given functions in parallel and returns the first error
/// encountered, waiting for all to finish (`run.Parallel`).
///
/// # Errors
/// The first error encountered, if any.
pub fn parallel<E: Send + 'static>(
    fs: Vec<Box<dyn FnOnce() -> Result<(), E> + Send>>,
) -> Result<(), E> {
    let total = fs.len();
    let results = std::sync::Mutex::new(Vec::<Result<(), E>>::new());
    std::thread::scope(|s| {
        for f in fs {
            s.spawn(|| {
                let r = f();
                results.lock().expect("results lock").push(r);
            });
        }
    });
    debug_assert_eq!(results.lock().expect("results lock").len(), total);
    results
        .into_inner()
        .expect("results lock")
        .into_iter()
        .find_map(|r| r.err())
        .map_or_else(|| Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;

    #[test]
    fn sequentially_returns_ok_when_no_functions() {
        assert!(sequentially::<String>(&[]).is_ok());
    }

    #[test]
    fn sequentially_returns_ok_when_all_succeed() {
        let fs: Vec<&dyn Fn() -> Result<(), String>> = vec![&|| Ok(()), &|| Ok(())];
        assert!(sequentially(&fs).is_ok());
    }

    #[test]
    fn sequentially_returns_the_first_failing_error() {
        let expected = "error in function 2".to_string();
        let f3 = || Err("error in function 3".to_string());
        let f2 = move || Err(expected.clone());
        let fs: Vec<&dyn Fn() -> Result<(), String>> = vec![&|| Ok(()), &f2, &f3];
        let err = sequentially(&fs).expect_err("must fail");
        assert_eq!(err, "error in function 2");
    }

    #[test]
    fn sequentially_does_not_run_functions_after_failure() {
        let expected = "error in function 1".to_string();
        let run_count = AtomicI32::new(0);
        let f1 = || {
            run_count.fetch_add(1, Ordering::SeqCst);
            Err::<(), String>(expected.clone())
        };
        let f2 = || {
            run_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let fs: Vec<&dyn Fn() -> Result<(), String>> = vec![&f1, &f2];
        let err = sequentially(&fs).expect_err("must fail");
        assert_eq!(err, "error in function 1");
        assert_eq!(run_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parallel_returns_ok_when_no_functions() {
        assert!(parallel::<String>(Vec::new()).is_ok());
    }

    #[test]
    fn parallel_returns_ok_when_all_succeed() {
        let fs: Vec<Box<dyn FnOnce() -> Result<(), String> + Send>> = vec![
            Box::new(|| Ok(())),
            Box::new(|| Ok(())),
            Box::new(|| Ok(())),
        ];
        assert!(parallel(fs).is_ok());
    }

    #[test]
    fn parallel_returns_an_error_when_one_fails() {
        let fs: Vec<Box<dyn FnOnce() -> Result<(), String> + Send>> = vec![
            Box::new(|| Ok(())),
            Box::new(|| Err("parallel error".to_string())),
            Box::new(|| Err("another error".to_string())),
        ];
        assert!(parallel(fs).is_err());
    }

    #[test]
    fn parallel_runs_all_functions_concurrently() {
        let entered = Arc::new(AtomicI32::new(0));
        let mut fs: Vec<Box<dyn FnOnce() -> Result<(), String> + Send>> = Vec::new();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        for _ in 0..3 {
            let entered = Arc::clone(&entered);
            let barrier = Arc::clone(&barrier);
            fs.push(Box::new(move || {
                let cur = entered.fetch_add(1, Ordering::SeqCst) + 1;
                barrier.wait();
                entered.fetch_sub(1, Ordering::SeqCst);
                assert!(cur <= 3);
                Ok(())
            }));
        }
        parallel(fs).expect("all ok");
        assert_eq!(entered.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn parallel_waits_for_all_before_returning() {
        let completed = Arc::new(AtomicI32::new(0));
        let mut fs: Vec<Box<dyn FnOnce() -> Result<(), String> + Send>> = Vec::new();
        fs.push(Box::new(|| Err("fast error".to_string())));
        let completed_slow = Arc::clone(&completed);
        fs.push(Box::new(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            completed_slow.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        let start = std::time::Instant::now();
        let r = parallel(fs);
        assert!(r.is_err());
        assert!(start.elapsed() >= std::time::Duration::from_millis(50));
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }
}
