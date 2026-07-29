//! MCP server binary: stdin/stdout JSON-RPC loop plus feature-crate composition.

use anyhow::Result;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::{self, BufReader, Stdout};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tools_mcp_core::{
    RpcResponse, read_mcp_message, should_skip_headers, write_mcp_payload_with_mode,
};
use tracing::{error, info};

use crate::composition::{InflightRegistry, JsonRpcId, build_tool_registry};
use crate::mcp_server::{
    ParseRpcRequestError, RpcBatchItem, RpcMessage, RpcRequest, StaticProtocolPayloads,
    dispatch_jsonrpc_batch, dispatch_jsonrpc_request, parse_rpc_message,
};

mod composition;
mod mcp_server;
mod ping;

type SharedWriter = Arc<Mutex<Stdout>>;
const MAX_INFLIGHT_DISPATCH_TASKS: usize = 64;

enum ServerControl {
    GracefulShutdown,
    AbortPendingTasks,
}

async fn write_response<T: serde::Serialize + ?Sized>(
    writer: &SharedWriter,
    response: &T,
    skip_headers: bool,
) -> Result<()> {
    let mut writer = writer.lock().await;
    write_mcp_payload_with_mode(&mut *writer, response, skip_headers).await
}

async fn write_payload<T: serde::Serialize + ?Sized>(
    writer: &SharedWriter,
    payload: &T,
    skip_headers: bool,
) -> Result<()> {
    let mut writer = writer.lock().await;
    write_mcp_payload_with_mode(&mut *writer, payload, skip_headers).await
}

fn request_requests_shutdown(req: &RpcRequest) -> bool {
    !req.is_notification && req.method_kind.is_shutdown()
}

fn batch_requests_shutdown(items: &[RpcBatchItem]) -> bool {
    items.iter().any(|item| match item {
        RpcBatchItem::Request(req) => request_requests_shutdown(req),
        RpcBatchItem::Response | RpcBatchItem::InvalidRequest { .. } => false,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let writer: SharedWriter = Arc::new(Mutex::new(stdout));
    let mut reader = BufReader::new(stdin);

    tools_mcp_local::start_search_cache_warmer();

    let registry = Arc::new(build_tool_registry());
    let tools = registry.list();
    let static_payloads = Arc::new(StaticProtocolPayloads::new(&tools)?);
    let inflight = InflightRegistry::default();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<ServerControl>();
    let mut tasks = JoinSet::new();
    let mut abort_pending_tasks = false;
    let abort_requested = Arc::new(AtomicBool::new(false));
    let task_limiter = Arc::new(Semaphore::new(MAX_INFLIGHT_DISPATCH_TASKS));

    'read_loop: loop {
        while let Some(join_result) = tasks.try_join_next() {
            if let Err(join_err) = join_result
                && !join_err.is_cancelled()
            {
                error!("dispatch task failed: {}", join_err);
            }
        }

        let message = tokio::select! {
            biased;
            Some(control) = control_rx.recv() => {
                match control {
                    ServerControl::GracefulShutdown => {
                        info!("shutdown requested");
                    }
                    ServerControl::AbortPendingTasks => {
                        abort_pending_tasks = true;
                    }
                }
                break 'read_loop;
            }
            read_result = read_mcp_message(&mut reader) => {
                match read_result {
                    Ok(Some(v)) => v,
                    Ok(None) => break 'read_loop,
                    Err(read_err) => {
                        error!("failed to read MCP message: {}", read_err.error);
                        let parse_error = RpcResponse::protocol_error(None, -32700, "Parse error");
                        let skip_headers = if read_err.response_has_headers {
                            false
                        } else {
                            should_skip_headers()
                        };
                        if let Err(write_err) = write_response(&writer, &parse_error, skip_headers).await {
                            error!("failed to write parse error response: {}", write_err);
                            abort_pending_tasks = true;
                            break 'read_loop;
                        }
                        if read_err.should_continue {
                            continue 'read_loop;
                        }
                        abort_pending_tasks = true;
                        break 'read_loop;
                    }
                }
            }
        };
        let line = message.body;

        let message_kind = match parse_rpc_message(&line) {
            Ok(RpcMessage::Request(r)) => {
                info!("Received request: method={}, id={:?}", &r.method, &r.id);
                RpcMessage::Request(r)
            }
            Ok(RpcMessage::Batch(items)) => {
                info!("Received batch request with {} item(s)", items.len());
                RpcMessage::Batch(items)
            }
            Ok(RpcMessage::Response) => {
                info!("Received JSON-RPC response");
                continue;
            }
            Err(ParseRpcRequestError::Parse(e)) => {
                error!("invalid json: {}", e);
                let response = RpcResponse::protocol_error(None, -32700, "Parse error");
                let skip_headers = if message.has_headers {
                    false
                } else {
                    should_skip_headers()
                };
                if let Err(write_err) = write_response(&writer, &response, skip_headers).await {
                    error!("failed to write parse error response: {}", write_err);
                    abort_pending_tasks = true;
                    break;
                }
                continue;
            }
            Err(ParseRpcRequestError::InvalidRequest { id, message: msg }) => {
                error!("invalid request: {}", msg);
                let response = RpcResponse::protocol_error(id, -32600, msg);
                let skip_headers = if message.has_headers {
                    false
                } else {
                    should_skip_headers()
                };
                if let Err(write_err) = write_response(&writer, &response, skip_headers).await {
                    error!("failed to write invalid request response: {}", write_err);
                    abort_pending_tasks = true;
                    break;
                }
                continue;
            }
        };

        let skip_headers = if message.has_headers {
            false
        } else {
            should_skip_headers()
        };

        match message_kind {
            RpcMessage::Request(req) if req.is_notification => {
                let _ = dispatch_jsonrpc_request(
                    req,
                    registry.as_ref(),
                    static_payloads.as_ref(),
                    &inflight,
                    None,
                )
                .await;
            }
            RpcMessage::Request(req) => {
                let should_stop_reading = request_requests_shutdown(&req);
                let Ok(task_permit) = Arc::clone(&task_limiter).acquire_owned().await else {
                    abort_pending_tasks = true;
                    break 'read_loop;
                };
                let registry = Arc::clone(&registry);
                let static_payloads = Arc::clone(&static_payloads);
                let writer = Arc::clone(&writer);
                let inflight = inflight.clone();
                let control_tx = control_tx.clone();
                let abort_requested = Arc::clone(&abort_requested);
                let cancellation_token =
                    req.method_kind.is_tool_call().then(CancellationToken::new);
                let inflight_guard = cancellation_token.as_ref().and_then(|token| {
                    req.id
                        .as_ref()
                        .and_then(JsonRpcId::from_value)
                        .map(|request_id| {
                            inflight.register(request_id.clone(), token.clone());
                            inflight.drop_on_completion(request_id)
                        })
                });

                tasks.spawn(async move {
                    let _task_permit = task_permit;
                    let _guard = inflight_guard;
                    let Some((resp, should_exit)) = dispatch_jsonrpc_request(
                        req,
                        registry.as_ref(),
                        static_payloads.as_ref(),
                        &inflight,
                        cancellation_token,
                    )
                    .await
                    else {
                        return;
                    };

                    info!("Sending response for request id: {:?}", resp.id());
                    if let Err(write_err) = write_response(&writer, &resp, skip_headers).await {
                        error!("failed to write MCP response: {}", write_err);
                        abort_requested.store(true, Ordering::SeqCst);
                        let _ = control_tx.send(ServerControl::AbortPendingTasks);
                        return;
                    }
                    info!("Response sent successfully");

                    if should_exit {
                        let _ = control_tx.send(ServerControl::GracefulShutdown);
                    }
                });

                if should_stop_reading {
                    info!("shutdown accepted");
                    break 'read_loop;
                }
            }
            RpcMessage::Batch(items) => {
                let should_stop_reading = batch_requests_shutdown(&items);
                let Ok(task_permit) = Arc::clone(&task_limiter).acquire_owned().await else {
                    abort_pending_tasks = true;
                    break 'read_loop;
                };
                let registry = Arc::clone(&registry);
                let static_payloads = Arc::clone(&static_payloads);
                let writer = Arc::clone(&writer);
                let inflight = inflight.clone();
                let control_tx = control_tx.clone();
                let abort_requested = Arc::clone(&abort_requested);

                tasks.spawn(async move {
                    let _task_permit = task_permit;
                    let Some((responses, should_exit)) = dispatch_jsonrpc_batch(
                        items,
                        registry.as_ref(),
                        static_payloads.as_ref(),
                        &inflight,
                    )
                    .await
                    else {
                        return;
                    };

                    info!("Sending batch response with {} item(s)", responses.len());
                    if let Err(write_err) = write_payload(&writer, &responses, skip_headers).await {
                        error!("failed to write MCP batch response: {}", write_err);
                        abort_requested.store(true, Ordering::SeqCst);
                        let _ = control_tx.send(ServerControl::AbortPendingTasks);
                        return;
                    }
                    info!("Response sent successfully");

                    if should_exit {
                        let _ = control_tx.send(ServerControl::GracefulShutdown);
                    }
                });

                if should_stop_reading {
                    info!("shutdown accepted");
                    break 'read_loop;
                }
            }
            RpcMessage::Response => continue,
        }
    }

    if abort_pending_tasks || abort_requested.load(Ordering::SeqCst) {
        tasks.abort_all();
    }

    while let Some(join_result) = tasks.join_next().await {
        if abort_requested.load(Ordering::SeqCst) {
            tasks.abort_all();
        }
        if let Err(join_err) = join_result
            && !join_err.is_cancelled()
        {
            error!("dispatch task failed: {}", join_err);
        }
    }

    Ok(())
}
