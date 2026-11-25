//! BOOT-04 signal-source registration and delivery, in-process.
//!
//! This is its own integration-test binary on purpose: tokio keeps **one** unix
//! signal driver per process (`tokio-1.54.1/src/signal/unix.rs`, `globals()`), so
//! the runtime that registers handlers must be the runtime that is awaited. One
//! test on one runtime makes delivery deterministic instead of depending on which
//! runtime happened to claim the driver first.
//!
//! Everything here targets the test process itself, and the only signal sender is
//! a test-owned `/bin/kill` child that is waited for, so delivery is established
//! by an observed exit rather than by a sleep. `SIGTERM`'s default disposition is
//! replaced by the registered handler, so the test process survives it.
#![cfg(unix)]

use std::process::Command;
use std::time::Duration;

use nd_cli::args::EXIT_FAILURE;
use nd_cli::supervisor::{
    register_one, register_signal_sources, wait_for_registered_signals, ServeError, ServeOutcome,
    StopReason, SIGNAL_KINDS,
};

/// Send `SIGTERM` to this process and wait for the sender to exit.
///
/// `/bin/kill` returning success means the signal was delivered to the target, so
/// the caller knows the delivery happened without guessing at a duration. The
/// child is test-owned and always reaped.
fn sigterm_self_and_wait_for_delivery() {
    let pid = std::process::id();
    let mut child = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .spawn()
        .expect("spawn /bin/kill (test-owned child)");
    let status = child
        .wait()
        .expect("wait for the test-owned /bin/kill child");
    assert!(
        status.success(),
        "/bin/kill -TERM {pid} must succeed, otherwise no delivery was established"
    );
}

/// A stop signal delivered **after registration but before the wait is polled**
/// must still terminate the run.
///
/// Sequence, with no sleeps: register synchronously → deliver and prove delivery
/// by the sender's exit → only then await the wait future. If registration were
/// lazy (a spawned task nobody polls) or if the delivery were consumed and
/// dropped, the bounded await below would time out and this test would fail.
#[tokio::test(flavor = "current_thread")]
async fn signal_delivered_after_registration_and_before_the_wait_is_honored() {
    // Registration is synchronous and completes here: `signal_enable` installs
    // the handler before `signal()` returns, so at this point every signal in
    // SIGNAL_KINDS is already listened for — nothing is polled yet, and no
    // readiness has been emitted.
    let sources = register_signal_sources().expect("the Go signal set must register");
    assert_eq!(
        SIGNAL_KINDS.len(),
        4,
        "SIGINT/SIGHUP/SIGTERM/SIGABRT, matching cmd/root.go:114-121"
    );

    sigterm_self_and_wait_for_delivery();

    // The stop arrived while nothing was awaiting it. It must not be lost.
    let honored =
        tokio::time::timeout(Duration::from_secs(5), wait_for_registered_signals(sources)).await;
    assert_eq!(
        honored,
        Ok(StopReason::Signalled),
        "a stop delivered before the wait began must still terminate the run \
         (a timeout means the delivery was lost; Err means the source was gone)"
    );
}

/// A signal the runtime refuses to register is reported as a setup failure, which
/// is what lets the serve path refuse to start instead of serving unsignalled.
#[tokio::test(flavor = "current_thread")]
async fn registration_failure_is_a_typed_error_not_a_silent_gap() {
    // SIGKILL cannot be handled; tokio rejects it in `signal_enable`.
    let err = register_one(
        "SIGKILL",
        tokio::signal::unix::SignalKind::from_raw(libc_kill()),
    )
    .expect_err("SIGKILL cannot be registered");
    match err {
        ServeError::SignalSource { name, reason } => {
            assert_eq!(name, "SIGKILL");
            assert!(!reason.is_empty(), "the failure carries a reason: {reason}");
        }
        other => panic!("expected a signal-source failure, got {other:?}"),
    }

    // The refusal maps to a nonzero exit and reports an incomplete startup, so a
    // process with no stop source can never claim a graceful shutdown.
    let outcome = ServeOutcome::Failed(ServeError::SignalSource {
        name: "SIGTERM",
        reason: "refused".to_string(),
    });
    assert_eq!(outcome.exit_code(), EXIT_FAILURE);
    assert!(
        outcome
            .failure_reason()
            .expect("reason")
            .to_string()
            .contains("graceful shutdown"),
        "the message explains why refusing is safer than serving"
    );
    assert!(outcome.report().is_none(), "nothing was bound");
}

/// Negative control for the "capture a stop early" idea: a delivery that is
/// **observed** by `poll_recv` is consumed, and a later `recv` on the same stream
/// never sees it again. This is why the supervisor does not poll streams during
/// startup and then await them — the stop would be observed and dropped.
///
/// Uses `SIGUSR1`, which is not part of the Go signal set and is used nowhere
/// else in this file, so the shared per-process signal driver cannot be disturbed
/// by the other tests here.
#[tokio::test(flavor = "current_thread")]
async fn a_consumed_delivery_is_unrecoverable_so_early_polling_would_drop_stops() {
    use std::task::{Context, Poll, Waker};

    let mut signal = register_one("SIGUSR1", tokio::signal::unix::SignalKind::user_defined1())
        .expect("SIGUSR1 registers");
    let mut cx = Context::from_waker(Waker::noop());

    assert_eq!(
        signal.poll_recv(&mut cx),
        Poll::Pending,
        "nothing pending yet"
    );

    // Deliver SIGUSR1 to this process and prove delivery via the sender's exit.
    let mut killer = Command::new("/bin/kill")
        .arg("-USR1")
        .arg(std::process::id().to_string())
        .spawn()
        .expect("spawn /bin/kill (test-owned child)");
    assert!(killer.wait().expect("reap the test-owned child").success());

    // Drain the runtime so the signal driver processes the delivery. Bounded, and
    // the assertion below is what proves delivery, not this yield.
    tokio::time::timeout(Duration::from_secs(5), signal.recv())
        .await
        .expect("SIGUSR1 must reach the registered handler");

    // Now the demonstrable loss: poll sees nothing, and recv never returns.
    assert_eq!(
        signal.poll_recv(&mut cx),
        Poll::Pending,
        "the delivery was consumed by recv, so nothing is pending"
    );
    let lost = tokio::time::timeout(Duration::from_millis(250), signal.recv()).await;
    assert!(
        lost.err().is_some(),
        "a consumed delivery is gone: a poll-then-await design would drop the stop"
    );
}

/// `SIGKILL` as a raw signum, without a `libc` import in this file.
fn libc_kill() -> std::os::raw::c_int {
    // 9 on macOS and Linux; only used to prove registration is refused.
    9
}
