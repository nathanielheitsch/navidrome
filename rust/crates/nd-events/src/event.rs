//! Event types — port of `server/events/events.go`.
//!
//! `Event` mirrors the Go interface: [`Event::name`] derives the SSE `event:`
//! field from the concrete type name (lower-camel), [`Event::data`] is the
//! JSON payload. Go derives both via reflection on `reflect.TypeOf(evt)`;
//! Rust does not have runtime type names, so each event implements
//! [`Event`] with an explicit type tag that reproduces the Go output
//! byte-for-byte.

use serde::Serialize;

/// Marker: refresh all instances of a resource (`events.Any`).
pub const ANY: &str = "*";

/// A resource-ids pair identifying the instances to refresh.
/// `RefreshResource` bundles [`Vec`]s keyed by resource name (order preserved
/// per key, keys sorted by `serde_json` map serialization).
#[derive(Debug, Clone, Serialize)]
pub struct RefreshResource {
    /// Resource name → ids to refresh.
    pub resources: Vec<(String, Vec<String>)>,
}

/// A single SSE event (`Event` interface in `events.go`).
pub trait Event {
    /// The `event:` field: the Go type name lower-camel-cased
    /// (`strings.TrimPrefix(reflect.TypeOf(evt).String(), "*events.")` +
    /// `unicode.ToLower` of the first rune).
    fn event_name(&self) -> &'static str;

    /// The `data:` field: the JSON encoding of the event.
    fn event_data(&self) -> String;
}

/// JSON-encode `value`, mirroring Go's `json.Marshal` output.
fn marshal<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("event serialization cannot fail")
}

/// `ScanStatus` — scanner progress (`events.go:42`).
#[derive(Debug, Clone, Serialize)]
pub struct ScanStatus {
    /// Whether a scan is running.
    pub scanning: bool,
    /// Number of files processed.
    pub count: i64,
    /// Number of folders processed.
    #[serde(rename = "folderCount")]
    pub folder_count: i64,
    /// Last scan error, if any.
    pub error: String,
    /// The scan type that triggered the event.
    #[serde(rename = "scanType")]
    pub scan_type: String,
    /// Elapsed scan time in nanoseconds (Go `time.Duration` JSON encoding).
    #[serde(rename = "elapsedTime")]
    pub elapsed_time: i64,
}

impl Event for ScanStatus {
    fn event_name(&self) -> &'static str {
        "scanStatus"
    }
    fn event_data(&self) -> String {
        marshal(self)
    }
}

/// `KeepAlive` — periodic ping (`events.go:51`).
#[derive(Debug, Clone, Serialize)]
pub struct KeepAlive {
    /// Unix timestamp of the tick.
    pub ts: i64,
}

impl Event for KeepAlive {
    fn event_name(&self) -> &'static str {
        "keepAlive"
    }
    fn event_data(&self) -> String {
        marshal(self)
    }
}

/// `ServerStart` — sent to each newly-connected client (`events.go:54`).
#[derive(Debug, Clone, Serialize)]
pub struct ServerStart {
    /// Server start time (RFC3339, Go `time.Time` JSON encoding).
    #[serde(rename = "startTime")]
    pub start_time: chrono::DateTime<chrono::Utc>,
    /// Server version string.
    pub version: String,
}

impl Event for ServerStart {
    fn event_name(&self) -> &'static str {
        "serverStart"
    }
    fn event_data(&self) -> String {
        marshal(self)
    }
}

/// `NowPlayingCount` — concurrent-streams count (`events.go:63`).
#[derive(Debug, Clone, Serialize)]
pub struct NowPlayingCount {
    /// Number of concurrent streams.
    pub count: i64,
}

impl Event for NowPlayingCount {
    fn event_name(&self) -> &'static str {
        "nowPlayingCount"
    }
    fn event_data(&self) -> String {
        marshal(self)
    }
}

impl RefreshResource {
    /// No-arg constructor matching Go's `&RefreshResource{}`:
    /// an absent map renders as `{"*":"*"}` (full refresh).
    pub fn new() -> Self {
        Self {
            resources: Vec::new(),
        }
    }

    /// Adds `ids` under `resource` (`RefreshResource.With`, `events.go:69`).
    ///
    /// Go appends to `rr.resources[resource]`; when `ids` is empty it first
    /// appends `Any` (`"*"`). The map is materialized at serialization time.
    pub fn with(mut self, resource: &str, ids: &[&str]) -> Self {
        let key = resource.to_string();
        if ids.is_empty() {
            self.push(&key, ANY);
        }
        for id in ids {
            self.push(&key, id);
        }
        self
    }

    fn push(&mut self, key: &str, id: &str) {
        if let Some((_, v)) = self.resources.iter_mut().find(|(k, _)| k == key) {
            v.push(id.to_string());
            return;
        }
        self.resources.push((key.to_string(), vec![id.to_string()]));
    }

    /// The `data:` field for this event (`RefreshResource.Data`, `events.go:80`).
    ///
    /// Go marshals `map[string][]string`; `serde_json` preserves insertion
    /// order for structs, so we sort keys to match Go's canonical map order.
    pub fn data(&self) -> String {
        if self.resources.is_empty() {
            return r#"{"*":"*"}"#.to_string();
        }
        let mut map = std::collections::BTreeMap::new();
        for (k, v) in &self.resources {
            map.insert(k.clone(), v.clone());
        }
        marshal(&map)
    }
}

impl Event for RefreshResource {
    fn event_name(&self) -> &'static str {
        "refreshResource"
    }
    fn event_data(&self) -> String {
        self.data()
    }
}

impl Default for RefreshResource {
    fn default() -> Self {
        Self::new()
    }
}
