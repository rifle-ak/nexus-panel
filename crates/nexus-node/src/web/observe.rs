//! Observability endpoints: a server's resource usage and console, and the
//! node's own usage.
//!
//! The console streams as server-sent events over a plain `fetch`, so the
//! browser can send its session header (an `EventSource` cannot). Each
//! event is one line of output; the stream follows the log until the
//! client goes away.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;

use super::{err_json, node_err_status, ApiError, S};
use crate::stats::{NodeSample, ResourceSample};

type ApiResult<T> = Result<T, (axum::http::StatusCode, Json<ApiError>)>;

/// The most console history a client may ask for, in bytes.
const MAX_TAIL_BYTES: u64 = 512 * 1024;
const DEFAULT_TAIL_BYTES: u64 = 64 * 1024;
/// How long a follower waits before looking for more output.
const FOLLOW_POLL: Duration = Duration::from_millis(250);

#[derive(Serialize)]
pub struct ContainerStatsResp {
    /// Whether the server is running and has been sampled.
    pub running: bool,
    pub current: Option<ResourceSample>,
    /// Recent samples, oldest first.
    pub history: Vec<ResourceSample>,
    /// Seconds between samples.
    pub interval_secs: u64,
}

/// A server's resource usage now and over the last few minutes.
pub(super) async fn api_container_stats(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<Json<ContainerStatsResp>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let current = s.monitor.usage(&id).await;
    Ok(Json(ContainerStatsResp {
        running: current.is_some(),
        current,
        history: s.monitor.history(&id).await,
        interval_secs: crate::stats::SAMPLE_INTERVAL.as_secs(),
    }))
}

#[derive(Deserialize)]
pub struct TailQuery {
    pub bytes: Option<u64>,
}

#[derive(Serialize)]
pub struct ConsoleTailResp {
    /// The tail of the console log, as it was written.
    pub text: String,
}

/// The last part of a server's console output.
pub(super) async fn api_console_tail(
    State(s): State<S>,
    Path(id): Path<String>,
    Query(q): Query<TailQuery>,
) -> ApiResult<Json<ConsoleTailResp>> {
    let bytes = q.bytes.unwrap_or(DEFAULT_TAIL_BYTES).clamp(1, MAX_TAIL_BYTES);
    let text = s
        .manager
        .console_tail(&id, bytes)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(ConsoleTailResp { text }))
}

/// Follow a server's console as server-sent events, one `line` event per
/// line of output. The stream starts at the current end of the log; the
/// client fetches history first with the tail endpoint.
pub(super) async fn api_console_stream(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let mut console = s
        .manager
        .attach_console(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);
    tokio::spawn(async move {
        // The runtime's reader opens the log at its tail minus a small
        // backlog; drain that so the stream begins with new output only.
        while let Ok(Some(_)) = console.read_line().await {}
        loop {
            match console.read_line().await {
                Ok(Some(line)) => {
                    let line = line.trim_end_matches(['\n', '\r']).to_string();
                    if tx.send(Ok(Event::default().event("line").data(line))).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    tokio::select! {
                        _ = tokio::time::sleep(FOLLOW_POLL) => {}
                        _ = tx.closed() => break,
                    }
                }
                Err(e) => {
                    let _ = tx.send(Ok(Event::default().event("error").data(e.to_string()))).await;
                    break;
                }
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keep-alive")))
}

#[derive(Serialize)]
pub struct NodeStatsResp {
    pub current: Option<NodeSample>,
    pub history: Vec<NodeSample>,
    /// Every running server's latest sample, with its name.
    pub servers: Vec<ServerUsage>,
    pub interval_secs: u64,
}

#[derive(Serialize)]
pub struct ServerUsage {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub usage: ResourceSample,
}

/// The node's usage now and recently, with every running server's share.
pub(super) async fn api_node_stats(State(s): State<S>) -> Json<NodeStatsResp> {
    let usage = s.monitor.all_usage().await;
    let mut servers: Vec<ServerUsage> = s
        .manager
        .list_containers()
        .await
        .into_iter()
        .filter_map(|c| {
            usage.get(&c.id).map(|u| ServerUsage {
                id: c.id.clone(),
                name: c.name.clone(),
                usage: u.clone(),
            })
        })
        .collect();
    servers.sort_by(|a, b| {
        b.usage
            .cpu_percent
            .partial_cmp(&a.usage.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Json(NodeStatsResp {
        current: s.monitor.node().await,
        history: s.monitor.node_history().await,
        servers,
        interval_secs: crate::stats::SAMPLE_INTERVAL.as_secs(),
    })
}
