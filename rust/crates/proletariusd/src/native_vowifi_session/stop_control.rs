//! Ephemeral exact-owner stop control. Never writes, replays, or resolves a
//! durable request ID. A published watch value is not a worker/join receipt.
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Body};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

pub(super) const READER_PATH: &str = "/api/reader/vowifi/stop-control";
pub(super) const MODULE_PATH: &str = "/api/vowifi/stop-control";
const INVALID: &str = "invalid_vowifi_stop_target";
pub(super) const STALE: &str = "vowifi_stop_target_stale";
pub(super) const UNCONFIRMED: &str = "vowifi_stop_unconfirmed";

pub(super) fn new_epoch() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "vowifi_stop_epoch_unavailable".to_owned())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Target {
    pub(super) scope: String,
    pub(super) daemon_epoch: String,
    pub(super) owner_id: String,
    pub(super) owner_generation: u64,
}
impl Target {
    /// Query parsing and body rejection are synchronous. POST deliberately has
    /// no JSON body: the owner receives its signal before the handler can await.
    pub(super) fn request<B: Body>(
        request: &Request<B>,
        scope: &str,
    ) -> Result<Self, &'static str> {
        if !matches!(*request.method(), Method::GET | Method::POST)
            || !request.body().is_end_stream()
            || request.body().size_hint().exact() != Some(0)
            || request
                .headers()
                .contains_key(hyper::header::TRANSFER_ENCODING)
            || request
                .headers()
                .get_all(hyper::header::CONTENT_LENGTH)
                .iter()
                .any(|v| v.as_bytes() != b"0")
            || request
                .headers()
                .get_all(hyper::header::CONTENT_LENGTH)
                .iter()
                .count()
                > 1
        {
            return Err(INVALID);
        }
        Self::query(request.uri().query(), scope)
    }
    fn query(query: Option<&str>, scope: &str) -> Result<Self, &'static str> {
        let query = query
            .filter(|q| {
                !q.is_empty()
                    && q.len() <= 1024
                    && !q.contains('#')
                    && q.split('&').all(|part| !part.is_empty())
            })
            .ok_or(INVALID)?;
        let bytes = query.as_bytes();
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'%'
                && !(bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                    && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
            {
                return Err(INVALID);
            }
        }
        let mut fields = std::collections::BTreeMap::new();
        // Keys and opaque values have one canonical wire encoding. URLComponents
        // emits this unreserved ASCII alphabet verbatim; percent aliases and +
        // must never create alternate spellings of the exact target.
        if bytes.iter().any(|b| *b == b'%' || *b == b'+') {
            return Err(INVALID);
        }
        for (key, value) in url::form_urlencoded::parse(bytes) {
            if !matches!(
                key.as_ref(),
                "scope" | "daemon_epoch" | "owner_id" | "owner_generation"
            ) || value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                || fields
                    .insert(key.into_owned(), value.into_owned())
                    .is_some()
            {
                return Err(INVALID);
            }
        }
        if fields.len() != 4 {
            return Err(INVALID);
        }
        let generation = fields.remove("owner_generation").ok_or(INVALID)?;
        if generation.starts_with('0') || !generation.bytes().all(|b| b.is_ascii_digit()) {
            return Err(INVALID);
        }
        let target = Self {
            scope: fields.remove("scope").ok_or(INVALID)?,
            daemon_epoch: fields.remove("daemon_epoch").ok_or(INVALID)?,
            owner_id: fields.remove("owner_id").ok_or(INVALID)?,
            owner_generation: generation.parse().map_err(|_| INVALID)?,
        };
        if target.scope != scope || target.owner_generation == 0 {
            return Err(INVALID);
        }
        Ok(target)
    }
    #[cfg(test)]
    pub(super) fn query_string(&self) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("scope", &self.scope)
            .append_pair("daemon_epoch", &self.daemon_epoch)
            .append_pair("owner_id", &self.owner_id)
            .append_pair("owner_generation", &self.owner_generation.to_string())
            .finish()
    }
}

#[derive(Clone, Serialize)]
pub(super) struct Projection {
    #[serde(flatten)]
    target: Target,
    signal_state: &'static str,
    receipt_durability: &'static str,
    owner_join: &'static str,
    cleanup: &'static str,
    restart_effect: &'static str,
    poll_after_ms: u64,
    message: String,
}
#[derive(Default)]
struct State {
    started: bool,
    published: bool,
    joined: bool,
    cleanup: bool,
    failed: bool,
    error: Option<String>,
    // This one retained task owns exact-record join/cleanup. HTTP tasks only
    // publish or sample, and never take/cancel this handle.
    task: Option<tokio::task::JoinHandle<()>>,
}
pub(super) struct Owner {
    pub(super) owner_id: String,
    state: Mutex<State>,
}
impl Default for Owner {
    fn default() -> Self {
        Self {
            owner_id: crate::snapshot::new_instance_id(),
            state: Mutex::new(State::default()),
        }
    }
}
impl Owner {
    #[cfg(test)]
    pub(super) fn test_poison(self: &Arc<Self>) {
        let owner = self.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = owner.state.lock().unwrap();
                panic!("injected control state poison");
            })
            .join()
            .is_err()
        );
    }
    pub(super) fn target(&self, scope: &str, epoch: &str, generation: u64) -> Target {
        Target {
            scope: scope.into(),
            daemon_epoch: epoch.into(),
            owner_id: self.owner_id.clone(),
            owner_generation: generation,
        }
    }
    pub(super) fn busy(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |s| s.started && !(s.joined && s.cleanup))
    }
    pub(super) fn started(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.started)
    }
    pub(super) fn begin<F>(
        self: &Arc<Self>,
        signal: F,
        join: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), &'static str>
    where
        F: FnOnce(),
    {
        {
            let mut state = self.state.lock().map_err(|_| UNCONFIRMED)?;
            if state.started {
                return Ok(());
            }
            state.started = true;
        }
        // Reserve once under the state lock, but never call an external
        // closure while holding it. Even an injected signal panic is fenced.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(signal)).is_err() {
            self.finish(false, false, Some("停止信号发布异常；清理未确认".into()));
            return Err(UNCONFIRMED);
        }
        let publication_confirmed = match self.state.lock() {
            Ok(mut state) => {
                state.published = true;
                true
            }
            Err(_) => false,
        };
        // Once the synchronous signal returned, no metadata-lock failure may
        // discard the captured join future. Always install its real owner.
        let completion = self.clone();
        let task = tokio::spawn(async move {
            // The retained supervisor observes the actual join owner's panic;
            // only HTTP observers are disposable. Never release on JoinError.
            if tokio::spawn(join).await.is_err() {
                completion.finish(
                    false,
                    false,
                    Some("停止join owner异常；资源保持隔离，清理未确认".into()),
                );
            }
        });
        let installed_confirmed = match self.state.lock() {
            Ok(mut state) => {
                state.task = Some(task);
                true
            }
            Err(poisoned) => {
                // Recover custody of the handle only. Do not clear poison or
                // use poisoned progress to acknowledge publication/cleanup.
                poisoned.into_inner().task = Some(task);
                false
            }
        };
        if publication_confirmed && installed_confirmed {
            Ok(())
        } else {
            Err(UNCONFIRMED)
        }
    }
    pub(super) fn finish(&self, joined: bool, cleanup: bool, error: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.joined = joined;
            state.cleanup = cleanup;
            state.failed = !joined || !cleanup;
            state.error = error;
        }
    }
    pub(super) fn project(&self, target: Target) -> Result<Projection, &'static str> {
        if target.owner_id != self.owner_id {
            return Err(STALE);
        }
        let state = self.state.lock().map_err(|_| UNCONFIRMED)?;
        Ok(Projection {
            target,
            signal_state: if state.published {
                "published"
            } else if state.failed {
                "unconfirmed"
            } else {
                "not_sent"
            },
            receipt_durability: "not_requested",
            owner_join: if state.joined {
                "joined"
            } else if state.failed {
                "failed"
            } else if state.started {
                "pending"
            } else {
                "unconfirmed"
            },
            cleanup: if state.cleanup {
                "confirmed"
            } else if state.failed {
                "failed"
            } else {
                "unconfirmed"
            },
            restart_effect: "none",
            poll_after_ms: if !state.started || state.failed || state.joined && state.cleanup {
                0
            } else {
                1500
            },
            message: state.error.clone().unwrap_or_else(|| {
                if !state.started {
                    "尚未通过临时控制发布停止；没有join或清理回执".into()
                } else if !state.published {
                    "临时停止已占位，信号发布尚未确认；没有join或清理回执".into()
                } else if state.joined && state.cleanup {
                    "本次运行的原会话已退出并释放资源；永久请求回执仍须独立核对".into()
                } else {
                    "已向原会话发布停止；尚未确认退出，未承诺持久回执或重启后停止".into()
                }
            }),
        })
    }
}
pub(super) type Retired = Option<(Target, Arc<Owner>)>;
pub(super) fn retire(retired: &mut Retired, owner: &Arc<Owner>, target: Target) {
    if owner.started() {
        *retired = Some((target, owner.clone()));
    }
}
pub(super) fn response(result: Result<Projection, &'static str>) -> Response<Full<Bytes>> {
    let mut response = match result {
        Ok(projection) => crate::json_response(
            if projection.poll_after_ms > 0 {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            &serde_json::json!({"stop_control": projection}),
        ),
        Err(code) => crate::json_response(
            if code == INVALID {
                StatusCode::BAD_REQUEST
            } else if code == STALE {
                StatusCode::CONFLICT
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            },
            &serde_json::json!({"code":code,"error":code}),
        ),
    };
    response.headers_mut().insert(
        hyper::header::CACHE_CONTROL,
        hyper::header::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod tests;

/// Exactly one harvester owns a module worker JoinHandle. An observer never
/// takes it, and absence of a handle is never converted into join success.
#[derive(Clone, Default)]
pub(super) struct TaskJoin(Arc<TaskJoinInner>);
#[derive(Default)]
struct TaskJoinInner {
    state: Mutex<TaskJoinState>,
    changed: tokio::sync::Notify,
}
#[derive(Default)]
struct TaskJoinState {
    started: bool,
    outcome: Option<bool>,
    harvester: Option<tokio::task::JoinHandle<()>>,
}
impl TaskJoin {
    pub(super) fn started(&self) -> bool {
        self.0.state.lock().map_or(true, |s| s.started)
    }
    pub(super) fn spawn(
        &self,
        work: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), &'static str> {
        let mut state = self.0.state.lock().map_err(|_| UNCONFIRMED)?;
        if state.started || state.outcome.is_some() {
            return Err(UNCONFIRMED);
        }
        state.started = true;
        let inner = self.0.clone();
        state.harvester = Some(tokio::spawn(async move {
            let joined = tokio::spawn(work).await.is_ok();
            if let Ok(mut state) = inner.state.lock() {
                state.outcome = Some(joined);
            }
            inner.changed.notify_waiters();
        }));
        Ok(())
    }
    // Only the synchronous zero-dispatch claim-release path may publish this.
    pub(super) fn confirm_unspawned(&self) -> Result<(), &'static str> {
        let mut state = self.0.state.lock().map_err(|_| UNCONFIRMED)?;
        if state.started {
            return Err(UNCONFIRMED);
        }
        state.outcome = Some(true);
        self.0.changed.notify_waiters();
        Ok(())
    }
    pub(super) async fn wait(&self) -> Result<bool, &'static str> {
        loop {
            let changed = self.0.changed.notified();
            {
                let state = self.0.state.lock().map_err(|_| UNCONFIRMED)?;
                if let Some(result) = state.outcome {
                    return Ok(result);
                }
                if !state.started {
                    return Err(UNCONFIRMED);
                }
            }
            changed.await;
        }
    }
}

#[cfg(test)]
pub(super) async fn test_head_request(state: crate::ServerState, path: &str) -> StatusCode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let token = state.app_token.clone().expect("private fixture token");
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(crate::serve_connection(server, state));
    let wire = format!(
        "HEAD {path} HTTP/1.1\r\nHost: localhost\r\nX-Proletarius-App-Token: {token}\r\nContent-Length: 0\r\n\r\n"
    );
    client.write_all(wire.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        client.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    task.await.unwrap().unwrap();
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("HEAD headers")
        + 4;
    assert!(response[split..].is_empty(), "HEAD must have no body");
    StatusCode::from_bytes(&response[9..12]).unwrap()
}
