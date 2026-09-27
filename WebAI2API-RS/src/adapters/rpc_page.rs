//! Bind a generic browser runtime page to the adapter-facing client trait.

use super::client::ClientFuture;
use super::{AdapterError, BrowserEvent, DownloadResult, PageClient};
use crate::browser_rpc::{BrowserRpc, PageOperation, RouteDecision};
use base64::Engine;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tokio::sync::Mutex;

pub struct RpcPage {
    rpc: BrowserRpc,
    browser_id: String,
    page_id: String,
    subscription: Mutex<Option<String>>,
}

impl RpcPage {
    pub fn new(rpc: BrowserRpc, browser_id: String, page_id: String) -> Self {
        Self {
            rpc,
            browser_id,
            page_id,
            subscription: Mutex::new(None),
        }
    }

    pub fn page_id(&self) -> &str {
        &self.page_id
    }
}

fn rpc_error(error: impl std::fmt::Display) -> AdapterError {
    AdapterError::new(error.to_string())
}

impl PageClient for RpcPage {
    fn call<'a>(&'a self, operations: Vec<Value>) -> ClientFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let operations: Vec<PageOperation> = operations
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<_, _>>()
                .map_err(rpc_error)?;
            let result = self
                .rpc
                .page_call(&self.page_id, operations, Duration::from_secs(600))
                .await
                .map_err(rpc_error)?;
            if let Some(index) = result.failed_index {
                return Err(AdapterError::new(format!(
                    "page.call operation {index}: {}",
                    result
                        .error
                        .unwrap_or_else(|| "unknown browser error".into())
                )));
            }
            Ok(result.results)
        })
    }

    fn subscribe_responses<'a>(&'a self) -> ClientFuture<'a, u64> {
        Box::pin(async move {
            let mut current = self.subscription.lock().await;
            if let Some(id) = current.take() {
                let _ = self.rpc.event_unsubscribe(&id).await;
            }
            let subscription = self
                .rpc
                .event_subscribe(
                    Some(&self.browser_id),
                    Some(&self.page_id),
                    vec!["response".into(), "route".into()],
                )
                .await
                .map_err(rpc_error)?;
            *current = Some(subscription.subscription_id);
            Ok(subscription.after_sequence)
        })
    }

    fn poll_events<'a>(&'a self, after_sequence: u64) -> ClientFuture<'a, Vec<BrowserEvent>> {
        Box::pin(async move {
            let batch = self
                .rpc
                .event_poll(after_sequence, Some(1024))
                .await
                .map_err(rpc_error)?;
            if batch.dropped != 0 {
                return Err(AdapterError::new(format!(
                    "browser event buffer dropped {} events",
                    batch.dropped
                )));
            }
            Ok(batch
                .events
                .into_iter()
                .filter_map(|event| {
                    let mut value = Value::Object(event.fields);
                    value["event"] = Value::String(event.event);
                    if let Some(sequence) = event.sequence {
                        value["sequence"] = Value::from(sequence);
                    }
                    BrowserEvent::from_json(&value)
                })
                .filter(|event| event.page_id.as_deref() == Some(&self.page_id))
                .collect())
        })
    }

    fn response_body<'a>(&'a self, response_id: &'a str) -> ClientFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let result = self
                .rpc
                .response_body(response_id, None)
                .await
                .map_err(rpc_error)?;
            let encoded = result
                .base64
                .ok_or_else(|| AdapterError::new("response body was not returned as base64"))?;
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(rpc_error)
        })
    }

    fn response_body_file<'a>(
        &'a self,
        response_id: &'a str,
        path: &'a Path,
    ) -> ClientFuture<'a, u64> {
        Box::pin(async move {
            let path = path
                .to_str()
                .ok_or_else(|| AdapterError::new("response output path is not UTF-8"))?;
            self.rpc
                .response_body(response_id, Some(path))
                .await
                .map(|result| result.bytes)
                .map_err(rpc_error)
        })
    }

    fn response_wait_finished<'a>(
        &'a self,
        response_id: &'a str,
        timeout_ms: u64,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.rpc
                .response_wait_finished(response_id, timeout_ms)
                .await
                .map_err(rpc_error)?;
            Ok(())
        })
    }

    fn route_install<'a>(&'a self, pattern: &'a str, timeout_ms: u64) -> ClientFuture<'a, String> {
        Box::pin(async move {
            self.rpc
                .route_install(&self.page_id, Some(pattern), Some(timeout_ms))
                .await
                .map(|result| result.route_id)
                .map_err(rpc_error)
        })
    }

    fn route_resolve<'a>(&'a self, route_token: &'a str, decision: Value) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let decision: RouteDecision = serde_json::from_value(decision).map_err(rpc_error)?;
            self.rpc
                .route_resolve(route_token, decision)
                .await
                .map_err(rpc_error)?;
            Ok(())
        })
    }

    fn route_remove<'a>(&'a self, route_id: &'a str) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.rpc.route_remove(route_id).await.map_err(rpc_error)?;
            Ok(())
        })
    }

    fn download_fetch<'a>(
        &'a self,
        url: &'a str,
        path: &'a Path,
        headers: Value,
        timeout_ms: u64,
    ) -> ClientFuture<'a, u64> {
        Box::pin(async move {
            self.download_fetch_info(url, path, headers, timeout_ms)
                .await
                .map(|result| result.bytes)
        })
    }

    fn download_fetch_info<'a>(
        &'a self,
        url: &'a str,
        path: &'a Path,
        headers: Value,
        timeout_ms: u64,
    ) -> ClientFuture<'a, DownloadResult> {
        Box::pin(async move {
            let path = path
                .to_str()
                .ok_or_else(|| AdapterError::new("download output path is not UTF-8"))?;
            let result = self
                .rpc
                .download_fetch(&self.page_id, url, path, headers, Some(timeout_ms))
                .await
                .map_err(rpc_error)?;
            let content_type = result.headers.as_object().and_then(|headers| {
                headers
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                    .and_then(|(_, value)| value.as_str().map(str::to_owned))
            });
            Ok(DownloadResult {
                bytes: result.bytes,
                content_type,
            })
        })
    }

    fn cookies_get<'a>(&'a self, urls: Vec<String>) -> ClientFuture<'a, Vec<Value>> {
        Box::pin(async move {
            self.rpc
                .cookies_get(&self.browser_id, urls)
                .await
                .map(|result| result.cookies)
                .map_err(rpc_error)
        })
    }
}
