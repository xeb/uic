use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio_tungstenite::tungstenite::Message;

use crate::error::{Result, UicError};

#[derive(Clone)]
pub struct CdpClient {
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>,
    next_id: Arc<AtomicU64>,
    load_notify: Arc<Notify>,
}

impl CdpClient {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| UicError::Cdp(format!("WebSocket connect failed ({ws_url}): {e}")))?;

        let (mut write, mut read) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let load_notify = Arc::new(Notify::new());

        // Writer loop
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if write.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
        });

        // Reader loop
        let pending_reader = Arc::clone(&pending);
        let load_notify_reader = Arc::clone(&load_notify);
        tokio::spawn(async move {
            while let Some(frame) = read.next().await {
                let text = match frame {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue,
                    Err(_) => break,
                };

                let Ok(val) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };

                if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
                    if let Some(sender) = pending_reader.lock().await.remove(&id) {
                        if let Some(err) = val.get("error") {
                            let _ = sender.send(Err(UicError::Cdp(err.to_string())));
                        } else {
                            let _ = sender.send(Ok(val.get("result").cloned().unwrap_or(Value::Null)));
                        }
                    }
                } else if let Some(method) = val.get("method").and_then(|v| v.as_str()) {
                    if method == "Page.loadEventFired" || method == "Page.frameStoppedLoading" {
                        load_notify_reader.notify_waiters();
                    }
                }
            }

            // Fail any remaining in-flight calls on disconnect
            let mut map = pending_reader.lock().await;
            for (_, sender) in map.drain() {
                let _ = sender.send(Err(UicError::Cdp("CDP WebSocket disconnected".into())));
            }
        });

        let client = Self {
            tx,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            load_notify,
        };

        // Enable Page, Runtime & DOM domains for load events, JS evaluation, and iframe inspection
        let _ = client.send("Page.enable", json!({})).await;
        let _ = client.send("Runtime.enable", json!({})).await;
        let _ = client.send("DOM.enable", json!({})).await;

        Ok(client)
    }

    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        self.send_session(None, method, params).await
    }

    pub async fn send_session(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            payload["sessionId"] = json!(sid);
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, resp_tx);

        self.tx
            .send(payload.to_string())
            .map_err(|_| UicError::Cdp("CDP writer channel closed".into()))?;

        tokio::time::timeout(Duration::from_secs(20), resp_rx)
            .await
            .map_err(|_| UicError::Timeout(format!("CDP call timed out: {method}")))?
            .map_err(|_| UicError::Cdp(format!("CDP response channel dropped: {method}")))?
    }

    pub async fn set_viewport(&self, width: u32, height: u32) -> Result<()> {
        self.send(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": 1,
                "mobile": false,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        let notified = self.load_notify.notified();
        let res = self.send("Page.navigate", json!({ "url": url })).await?;
        if let Some(err_text) = res.get("errorText").and_then(|v| v.as_str()) {
            return Err(UicError::Cdp(format!("navigation error: {err_text}")));
        }
        // Wait for load event (or short timeout for SPA hash/in-page navigations)
        let _ = tokio::time::timeout(Duration::from_secs(8), notified).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        Ok(())
    }

    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        self.evaluate_in_session(None, expression).await
    }

    pub async fn evaluate_in_session(
        &self,
        session_id: Option<&str>,
        expression: &str,
    ) -> Result<Value> {
        let res = self
            .send_session(
                session_id,
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;

        if let Some(exc) = res.get("exceptionDetails") {
            return Err(UicError::Cdp(format!("JS exception: {exc}")));
        }
        Ok(res
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Discovers visible cross-origin iframe targets attached to this page, returning
    /// `(session_id, offset_x, offset_y)` for each iframe whose box model can be resolved.
    pub async fn attach_iframe_targets(&self) -> Vec<(String, f64, f64)> {
        let Ok(targets_val) = self.send("Target.getTargets", json!({})).await else {
            return Vec::new();
        };
        let Some(infos) = targets_val.get("targetInfos").and_then(|v| v.as_array()) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for info in infos {
            let ttype = info.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if ttype != "iframe" {
                continue;
            }
            let Some(target_id) = info.get("targetId").and_then(|v| v.as_str()) else {
                continue;
            };
            // Resolve iframe viewport offset via DOM.getFrameOwner + DOM.getBoxModel
            let Ok(owner) = self
                .send("DOM.getFrameOwner", json!({ "frameId": target_id }))
                .await
            else {
                continue;
            };
            let Some(backend_node_id) = owner.get("backendNodeId").and_then(|v| v.as_i64()) else {
                continue;
            };
            let Ok(box_model) = self
                .send("DOM.getBoxModel", json!({ "backendNodeId": backend_node_id }))
                .await
            else {
                continue;
            };
            let Some(content) = box_model
                .pointer("/model/content")
                .and_then(|v| v.as_array())
            else {
                continue;
            };
            if content.len() < 6 {
                continue;
            }
            let ox = content[0].as_f64().unwrap_or(0.0);
            let oy = content[1].as_f64().unwrap_or(0.0);
            let w = content[2].as_f64().unwrap_or(0.0) - ox;
            let h = content[5].as_f64().unwrap_or(0.0) - oy;
            if w < 10.0 || h < 10.0 {
                continue;
            }

            if let Ok(attached) = self
                .send(
                    "Target.attachToTarget",
                    json!({ "targetId": target_id, "flatten": true }),
                )
                .await
            {
                if let Some(sid) = attached.get("sessionId").and_then(|v| v.as_str()) {
                    out.push((sid.to_string(), ox, oy));
                }
            }
        }
        out
    }

    pub async fn capture_screenshot_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        let res = self
            .send(
                "Page.captureScreenshot",
                json!({
                    "format": "jpeg",
                    "quality": quality.clamp(20, 95),
                    "fromSurface": true,
                }),
            )
            .await?;

        let b64 = res
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| UicError::Cdp("missing screenshot data in CDP response".into()))?;

        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| UicError::Cdp(format!("base64 decode screenshot failed: {e}")))
    }

    pub async fn click_point(&self, x: f64, y: f64) -> Result<()> {
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": x,
                "y": y,
            }),
        )
        .await?;
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mousePressed",
                "x": x,
                "y": y,
                "button": "left",
                "clickCount": 1,
            }),
        )
        .await?;
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseReleased",
                "x": x,
                "y": y,
                "button": "left",
                "clickCount": 1,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.send("Input.insertText", json!({ "text": text }))
            .await?;
        Ok(())
    }

    pub async fn press_key(&self, key: &str) -> Result<()> {
        let (code, vk, text) = match key {
            "Enter" => ("Enter", 13, Some("\r")),
            "Escape" => ("Escape", 27, None),
            "Tab" => ("Tab", 9, None),
            "Backspace" => ("Backspace", 8, None),
            other => (other, 0, None),
        };
        let mut down = json!({
            "type": "rawKeyDown",
            "key": key,
            "code": code,
            "windowsVirtualKeyCode": vk,
            "nativeVirtualKeyCode": vk,
        });
        if let Some(t) = text {
            down["text"] = json!(t);
        }
        self.send("Input.dispatchKeyEvent", down).await?;
        self.send(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": vk,
                "nativeVirtualKeyCode": vk,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn scroll(&self, x: f64, y: f64, delta_y: f64) -> Result<()> {
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseWheel",
                "x": x,
                "y": y,
                "deltaX": 0.0,
                "deltaY": delta_y,
            }),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(120)).await;
        Ok(())
    }
}
