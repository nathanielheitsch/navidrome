//! Broker — port of `server/events/sse.go` (the broadcast half; the wire
//! half is [`crate::sse`]).
//!
//! Go's `broker` runs `listen()` in a goroutine, multiplexing `publish`,
//! `subscribing`, `unsubscribing` and a keep-alive ticker with `select`.
//! Rust has no channel `select`, so the hub is a `tokio` task fed by one
//! `mpsc` command queue plus a keep-alive `interval`; the observable
//! semantics (per-client ordering, drop-on-full inbox, `shouldSend`
//! filtering, monotonically increasing event ids, `serverStart` greeting
//! on subscribe) are preserved.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};

use crate::event::{Event, KeepAlive, ServerStart};
use crate::sse::KEEP_ALIVE_FREQUENCY;

/// Per-client message queue capacity (`bufferSize`, `sse.go:30`).
pub const BUFFER_SIZE: usize = 1;

/// Client identity — the Go `client` struct minus the channel
/// (`sse.go:42`); the inbox is held by [`Subscription`].
#[derive(Debug, Clone)]
pub struct Client {
    /// Random connection id (`id.NewRandom()`).
    pub id: String,
    /// Remote address (`r.RemoteAddr`).
    pub address: String,
    /// Authenticated username (`user.UserName`).
    pub username: String,
    /// `X-ND-Client-Unique-Id` header/cookie value.
    pub client_unique_id: String,
    /// Trace label (`sse.go:179-183`); the plain (non-trace) form.
    pub display_string: String,
}

impl Client {
    /// Builds a client identity with a fresh random id (`subscribe`,
    /// `sse.go:168`); the inbox pair is allocated by [`Broker::subscribe`].
    pub fn new(id: &str, username: &str, address: &str, client_unique_id: &str) -> Self {
        // Go appends the user agent only at trace level (`sse.go:179-183`);
        // the plain form is the closest portable default.
        let display_string = format!("{id} ({username} - {address} - {client_unique_id})");
        Self {
            id: id.to_string(),
            address: address.to_string(),
            username: username.to_string(),
            client_unique_id: client_unique_id.to_string(),
            display_string,
        }
    }
}

/// One SSE frame — the Go `message` struct (`sse.go:33`). Wire encoding
/// lives in [`crate::sse::write_event`].
#[derive(Debug, Clone)]
pub struct Frame {
    /// Monotonic SSE `id:` (`getNextEventId`, `sse.go:220`).
    pub id: u64,
    /// `event:` field — the event type name.
    pub event: String,
    /// `data:` field — JSON payload.
    pub data: String,
    /// Filter context from the publishing request (`shouldSend`, `sse.go:196`).
    pub sender: SenderCtx,
}

/// The `broadcastToAll` / `request.*` values `shouldSend` reads from the
/// sender context (`events.go:15`, `model/request/request.go`).
#[derive(Debug, Clone, Default)]
pub struct SenderCtx {
    /// `ctx.Value(broadcastToAll)` — send to every client regardless of
    /// user/client.
    pub broadcast_to_all: bool,
    /// `request.ClientUniqueIdFrom(ctx)`.
    pub client_unique_id: Option<String>,
    /// `request.UsernameFrom(ctx)`.
    pub username: Option<String>,
}

/// A live client connection — the inbox plus the detach handle.
pub struct Subscription {
    /// The registered client identity.
    pub client: Client,
    /// Frames addressed to this client; drops frames when full
    /// (`sendOrDrop`, `sse.go:272`).
    pub rx: mpsc::Receiver<Frame>,
    /// Detach handle; [`Subscription::unsubscribe`] feeds the hub.
    detach: oneshot::Sender<String>,
}

impl Subscription {
    /// Detaches the client: the hub stops delivering to it
    /// (`unsubscribe`, `sse.go:192`).
    pub fn unsubscribe(self) {
        let _ = self.detach.send(self.client.id.clone());
    }
}

/// Commands into the hub task (`broker.listen`, `sse.go:213`).
enum Cmd {
    /// `case c := <-b.subscribing`
    Subscribe {
        client: Client,
        inbox: mpsc::Sender<Frame>,
        ack: oneshot::Sender<()>,
    },
    /// `case c := <-b.unsubscribing`
    Unsubscribe { id: String },
    /// `case msg := <-b.publish`
    Publish { frame: Frame },
}

/// The broker — the Go `Broker`/`broker` pair (`sse.go:19,56`).
#[derive(Debug)]
pub struct Broker {
    tx: mpsc::Sender<Cmd>,
}

impl Clone for Broker {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl Broker {
    /// Creates a running broker. Go obtains it via `singleton.GetInstance`
    /// (`sse.go:67`); per the #S1 design freeze the caller constructs it
    /// explicitly and wires it where needed.
    ///
    /// `server_start`/`version` back the `serverStart` greeting
    /// (`consts.ServerStart`, `consts.Version`, `sse.go:234-235`).
    pub fn new(server_start: chrono::DateTime<chrono::Utc>, version: &str) -> Self {
        // Buffered like Go's `make(messageChan, 2)` (`sse.go:71`).
        let (tx, rx) = mpsc::channel(2);
        let greet = ServerStart {
            start_time: server_start,
            version: version.to_string(),
        };
        tokio::spawn(hub_task(rx, greet));
        Self { tx }
    }

    /// Publishes an event to the matching clients (`SendMessage`, `sse.go:87`).
    pub async fn send(&self, sender: SenderCtx, event: &dyn Event) {
        let frame = Frame {
            id: 0, // assigned by the hub (`getNextEventId`)
            event: event.event_name().to_string(),
            data: event.event_data(),
            sender,
        };
        // Go blocks on `b.publish <- msg` (buffered 2); we match the buffer
        // and drop only if the hub task is gone.
        let _ = self.tx.send(Cmd::Publish { frame }).await;
    }

    /// Publishes to every client (`SendBroadcastMessage`, `sse.go:82`).
    pub async fn send_broadcast(&self, event: &dyn Event) {
        let sender = SenderCtx {
            broadcast_to_all: true,
            ..SenderCtx::default()
        };
        self.send(sender, event).await;
    }

    /// Attaches a new client and greets it with `serverStart`
    /// (`subscribe`, `sse.go:168` + `sse.go:233-236`). Resolves once the hub
    /// registered the client, so subscriptions are ordered.
    pub async fn subscribe(&self, client: Client) -> Subscription {
        let (inbox, rx) = mpsc::channel(BUFFER_SIZE);
        let (ack_tx, ack_rx) = oneshot::channel();
        let (detach_tx, detach_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(Cmd::Subscribe {
                client: client.clone(),
                inbox,
                ack: ack_tx,
            })
            .await;
        let _ = ack_rx.await;
        // Route the detach through the same queue so the hub never sees
        // commands out of order.
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Ok(gone) = detach_rx.await {
                let _ = tx.send(Cmd::Unsubscribe { id: gone }).await;
            }
        });
        Subscription {
            client,
            rx,
            detach: detach_tx,
        }
    }
}

/// Hub loop — `broker.listen` (`sse.go:213`).
async fn hub_task(mut rx: mpsc::Receiver<Cmd>, greet: ServerStart) {
    let mut clients: HashMap<String, (Client, mpsc::Sender<Frame>)> = HashMap::new();
    let mut event_id: u64 = 0;

    let mut keep_alive = tokio::time::interval(KEEP_ALIVE_FREQUENCY);
    keep_alive.tick().await; // first tick fires immediately; discard

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(Cmd::Subscribe { client, inbox, ack }) => {
                        // A new client has connected. Register their message
                        // channel and send a serverStart event to them
                        // (`sse.go:227-236`).
                        let frame = Frame {
                            id: 0,
                            event: greet.event_name().to_string(),
                            data: greet.event_data(),
                            sender: SenderCtx::default(),
                        };
                        send_or_drop(&inbox, frame);
                        clients.insert(client.id.clone(), (client, inbox));
                        let _ = ack.send(());
                    }
                    Some(Cmd::Unsubscribe { id }) => {
                        // A client has detached, and we want to stop sending
                        // them messages (`sse.go:238-243`).
                        clients.remove(&id);
                    }
                    Some(Cmd::Publish { mut frame }) => {
                        // We got a new event from the outside! Send it to all
                        // connected clients that should receive it
                        // (`sse.go:245-255`).
                        frame.id = {
                            event_id += 1;
                            event_id
                        };
                        for (client, inbox) in clients.values() {
                            if should_send(&frame, client) {
                                send_or_drop(inbox, frame.clone());
                            }
                        }
                    }
                    None => break,
                }
            }
            _ = keep_alive.tick() => {
                // Send a keep alive message every 15 seconds to all connected
                // clients (`sse.go:257-267`).
                if clients.is_empty() {
                    continue;
                }
                let mut frame = Frame {
                    id: 0,
                    event: KeepAlive { ts: 0 }.event_name().to_string(),
                    data: KeepAlive { ts: chrono::Utc::now().timestamp() }.event_data(),
                    sender: SenderCtx::default(),
                };
                frame.id = {
                    event_id += 1;
                    event_id
                };
                for (_, inbox) in clients.values() {
                    send_or_drop(inbox, frame.clone());
                }
            }
        }
    }
}

/// `shouldSend` (`sse.go:196`): decides whether `frame` reaches `client`.
pub fn should_send(frame: &Frame, client: &Client) -> bool {
    let ctx = &frame.sender;
    if ctx.broadcast_to_all {
        return true;
    }
    let Some(sender_client_unique_id) = ctx.client_unique_id.as_deref() else {
        // `!originatedFromClient` — no clientUniqueId on the sender ctx.
        return true;
    };
    if client.client_unique_id == sender_client_unique_id {
        return false;
    }
    if let Some(username) = ctx.username.as_deref() {
        return username == client.username;
    }
    true
}

/// `sendOrDrop` (`sse.go:272`): a full inbox drops the frame instead of
/// blocking the hub (`select { case c.msgC <- msg: default: }`).
fn send_or_drop(inbox: &mpsc::Sender<Frame>, frame: Frame) {
    let _ = inbox.try_send(frame);
}

/// No-op broker — `NoopBroker` (`sse.go:282`).
#[derive(Debug, Clone, Default)]
pub struct NoopBroker;

impl NoopBroker {
    /// Swallows the event (`noopBroker.SendMessage`, `sse.go:292`).
    pub async fn send(&self, _sender: SenderCtx, _event: &dyn Event) {}

    /// Swallows the event (`noopBroker.SendBroadcastMessage`, `sse.go:290`).
    pub async fn send_broadcast(&self, _event: &dyn Event) {}
}
