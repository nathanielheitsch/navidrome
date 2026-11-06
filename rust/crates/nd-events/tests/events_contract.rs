//! #11 contract tests — SSE wire format, fan-out filtering, and lifecycle.
//! Facts pinned against `sse.go` / `events.go` line numbers in doc comments.

use nd_events::broker::{should_send, Broker, Client, Frame, SenderCtx};
use nd_events::event::{Event, KeepAlive, RefreshResource, ScanStatus, ServerStart, ANY};
use nd_events::sse::{write_event, KEEP_ALIVE_FREQUENCY, WRITE_TIME_OUT};

use chrono::Utc;
use tokio::time::{sleep, Duration};

/// `keepAliveFrequency` 15s (sse.go:26) < `writeTimeOut` 20s (sse.go:28).
#[test]
fn constants_match_go() {
    assert_eq!(KEEP_ALIVE_FREQUENCY.as_secs(), 15);
    assert_eq!(WRITE_TIME_OUT.as_secs(), 20);
    assert!(
        WRITE_TIME_OUT.as_secs() > KEEP_ALIVE_FREQUENCY.as_secs(),
        "sse.go:28 comment: writeTimeOut must exceed keepAliveFrequency"
    );
}

/// Event names are the Go type names lower-camel-cased (events.go reflection).
#[test]
fn event_names_match_go() {
    let keep = KeepAlive { ts: 1 };
    assert_eq!(keep.event_name(), "keepAlive");
    let scan = ScanStatus {
        scanning: true,
        count: 0,
        folder_count: 0,
        error: "".to_string(),
        scan_type: "".to_string(),
        elapsed_time: 0,
    };
    assert_eq!(scan.event_name(), "scanStatus");
    let rr = RefreshResource::new();
    assert_eq!(rr.event_name(), "refreshResource");
    let greet = ServerStart {
        start_time: Utc::now(),
        version: "x".to_string(),
    };
    assert_eq!(greet.event_name(), "serverStart");
    assert_eq!(ANY, "*");
}

/// `writeEvent` (sse.go:103): `id: %d\nevent: %s\ndata: %s\n\n` byte-exact.
#[test]
fn frame_wire_format() {
    let frame = Frame {
        id: 42,
        event: "scanStatus".to_string(),
        data: r#"{"scanning":true}"#.to_string(),
        sender: SenderCtx::default(),
    };
    let text = write_event(&frame);
    assert_eq!(
        text,
        "id: 42\nevent: scanStatus\ndata: {\"scanning\":true}\n\n"
    );
}

/// `shouldSend` (sse.go:196) boundary table in Go's evaluation order.
#[test]
fn should_send_table() {
    let client = Client {
        id: "c1".to_string(),
        address: "1.2.3.4".to_string(),
        username: "u".to_string(),
        client_unique_id: "uid1".to_string(),
        display_string: "".to_string(),
    };

    let bcast = Frame {
        id: 1,
        event: "e".to_string(),
        data: "d".to_string(),
        sender: SenderCtx {
            broadcast_to_all: true,
            ..SenderCtx::default()
        },
    };
    assert!(should_send(&bcast, &client));

    let origin = Frame {
        id: 1,
        event: "e".to_string(),
        data: "d".to_string(),
        sender: SenderCtx {
            client_unique_id: Some("uid1".to_string()),
            ..SenderCtx::default()
        },
    };
    assert!(!should_send(&origin, &client));

    let other_user = Frame {
        id: 1,
        event: "e".to_string(),
        data: "d".to_string(),
        sender: SenderCtx {
            client_unique_id: Some("uid2".to_string()),
            username: Some("u".to_string()),
            ..SenderCtx::default()
        },
    };
    assert!(should_send(&other_user, &client));

    let wrong_user = Frame {
        id: 1,
        event: "e".to_string(),
        data: "d".to_string(),
        sender: SenderCtx {
            client_unique_id: Some("uid2".to_string()),
            username: Some("other".to_string()),
            ..SenderCtx::default()
        },
    };
    assert!(!should_send(&wrong_user, &client));

    let server = Frame {
        id: 1,
        event: "e".to_string(),
        data: "d".to_string(),
        sender: SenderCtx::default(),
    };
    assert!(should_send(&server, &client));
}

/// `RefreshResource.Data` (events.go:80): empty → `{"*":"*"}`; keys sorted
/// like Go map marshalling.
#[test]
fn refresh_resource_payloads() {
    let empty = RefreshResource::new();
    assert_eq!(empty.event_data(), r#"{"*":"*"}"#);

    let rr = RefreshResource::with(RefreshResource::new(), "album", &["1", "2"]);
    assert_eq!(rr.event_data(), r#"{"album":["1","2"]}"#);

    let any = RefreshResource::with(RefreshResource::new(), "album", &[]);
    assert_eq!(any.event_data(), r#"{"album":["*"]}"#);
}

/// Go marshals the lowerCamel field names (`folderCount`, `scanType`,
/// `elapsedTime` — events.go:42).
#[test]
fn scan_status_payload_keys() {
    let scan = ScanStatus {
        scanning: false,
        count: 7,
        folder_count: 2,
        error: "".to_string(),
        scan_type: "Full".to_string(),
        elapsed_time: 1234567890,
    };
    let data = scan.event_data();
    assert!(data.contains("\"folderCount\":2"));
    assert!(data.contains("\"scanType\":\"Full\""));
    assert!(data.contains("\"elapsedTime\":1234567890"));
}

/// subscribe() greets with `serverStart` id 0 (sse.go:227-236) and returns the
/// real client identity.
#[tokio::test]
async fn subscribe_greeting() {
    let broker = Broker::new(Utc::now(), "0.0.0-test");
    let client = Client::new("id-1", "user", "127.0.0.1", "uid-1");
    let mut sub = broker.subscribe(client).await;
    assert_eq!(sub.client.username, "user");
    assert_eq!(sub.client.client_unique_id, "uid-1");

    let frame = sub.rx.recv().await;
    if frame.is_none() {
        panic!("no greeting");
    }
    let f = frame.expect("greeting");
    assert_eq!(f.id, 0);
    assert_eq!(f.event, "serverStart");
    assert!(f.data.contains("\"startTime\""));
    assert!(f.data.contains("\"version\":\"0.0.0-test\""));
}

/// Broadcast reaches every client with one shared id (sse.go:82 + fan-out
/// sse.go:245-255).
#[tokio::test]
async fn broadcast_fan_out() {
    let broker = Broker::new(Utc::now(), "v");
    let c1 = Client::new("id-1", "u1", "1", "uid-1");
    let c2 = Client::new("id-2", "u2", "2", "uid-2");
    let mut sub1 = broker.subscribe(c1).await;
    let mut sub2 = broker.subscribe(c2).await;
    let g1 = sub1.rx.recv().await;
    let g2 = sub2.rx.recv().await;
    assert!(g1.is_some() && g2.is_some(), "greetings must arrive");

    broker.send_broadcast(&KeepAlive { ts: 9 }).await;
    sleep(Duration::from_millis(5)).await;
    let a = sub1.rx.try_recv();
    let b = sub2.rx.try_recv();
    if a.is_err() || b.is_err() {
        panic!("both clients must get the broadcast");
    }
    let f1o = a.ok();
    let f2o = b.ok();
    match (f1o, f2o) {
        (Some(f1), Some(f2)) => {
            assert_eq!(f1.id, f2.id);
            assert_eq!(f1.event, "keepAlive");
            assert_eq!(f1.data, f2.data);
        }
        _ => panic!("both clients must get the broadcast"),
    }
}

/// An event originated by a client is NOT echoed to that client but reaches
/// others (shouldSend at fan-out, sse.go:251).
#[tokio::test]
async fn originator_skipped() {
    let broker = Broker::new(Utc::now(), "v");
    let c1 = Client::new("id-1", "u1", "1", "uid-1");
    let c2 = Client::new("id-2", "u2", "2", "uid-2");
    let mut sub1 = broker.subscribe(c1).await;
    let mut sub2 = broker.subscribe(c2).await;
    let g1 = sub1.rx.recv().await;
    let g2 = sub2.rx.recv().await;
    assert!(g1.is_some() && g2.is_some(), "greetings must arrive");

    let sender = SenderCtx {
        client_unique_id: Some("uid-1".to_string()),
        ..SenderCtx::default()
    };
    broker
        .send(
            sender,
            &ScanStatus {
                scanning: false,
                count: 1,
                folder_count: 0,
                error: "".to_string(),
                scan_type: "".to_string(),
                elapsed_time: 0,
            },
        )
        .await;
    sleep(Duration::from_millis(5)).await;

    // c1 inbox: greeting retained, its own event dropped at fan-out.
    let own = sub1.rx.try_recv();
    if let Ok(extra) = own {
        panic!("c1 must not receive its own event: {0}", extra.event)
    }

    // c2 receives greeting plus the scanStatus.
    let theirs = sub2.rx.try_recv();
    if theirs.is_err() {
        panic!("c2 must get the scanStatus");
    }
    match theirs.ok() {
        Some(f) => {
            assert_eq!(f.event, "scanStatus");
            assert_eq!(f.id, 1);
        }
        None => panic!("c2 must get the scanStatus"),
    }
}

/// BUFFER_SIZE=1 (sse.go:30): an undrained client has later frames dropped by
/// `sendOrDrop` default (sse.go:272), never blocking the hub.
#[tokio::test]
async fn full_inbox_drops_frames() {
    let broker = Broker::new(Utc::now(), "v");
    let c1 = Client::new("id-1", "u1", "1", "uid-1");
    let mut sub1 = broker.subscribe(c1).await;
    // Never drained: greeting occupies the size-1 inbox.

    broker.send_broadcast(&KeepAlive { ts: 1 }).await;
    broker.send_broadcast(&KeepAlive { ts: 2 }).await;
    broker.send_broadcast(&KeepAlive { ts: 3 }).await;
    sleep(Duration::from_millis(5)).await;

    // Drain: greeting still held, all broadcasts were dropped.
    let mut frames: Vec<Frame> = Vec::new();
    loop {
        let v = sub1.rx.try_recv();
        if v.is_err() {
            break;
        }
        match v.ok() {
            Some(f) => frames.push(f),
            None => break,
        }
    }
    assert_eq!(frames.len(), 1, "greeting only; broadcasts dropped");
    assert_eq!(frames[0].id, 0);
    assert_eq!(frames[0].event, "serverStart");
}

/// unsubscribe detaches the client: the hub stops sending (sse.go:238-243).
#[tokio::test]
async fn unsubscribe_stops_frames() {
    // unsubscribe consumes the Subscription (Rust-level proof that no further
    // frame can reach c1: the inbox receiver is destroyed with it) and routes
    // Unsubscribe through the hub queue; c2 proves the hub stays alive so the
    // later broadcast is genuinely skipped for c1 (sse.go:238-243).
    let broker = Broker::new(Utc::now(), "v");
    let c1 = Client::new("id-1", "u1", "1", "uid-1");
    let c2 = Client::new("id-2", "u2", "2", "uid-2");
    let mut sub1 = broker.subscribe(c1).await;
    let mut sub2 = broker.subscribe(c2).await;
    let _g1 = sub1.rx.recv().await;
    let _g2 = sub2.rx.recv().await;

    sub1.unsubscribe();
    sleep(Duration::from_millis(5)).await;

    broker.send_broadcast(&KeepAlive { ts: 2 }).await;
    sleep(Duration::from_millis(5)).await;

    let f2 = sub2.rx.try_recv();
    match f2.ok() {
        Some(f) => assert_eq!(f.event, "keepAlive"),
        None => panic!("hub must stay alive after unsubscribe"),
    }
}
