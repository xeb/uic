use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use image::{imageops, Rgb, RgbImage};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::decisions::{DecisionsClient, Question};
use crate::error::{Result, UicError};
use crate::vision;

pub const DEFAULT_VNC_URL: &str = "vnc://127.0.0.1:5900";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VncClickOutcome {
    pub target: String,
    pub x: u16,
    pub y: u16,
    pub coarse_cell: String,
    pub coarse_confidence: f64,
    pub fine_cell: String,
    pub fine_confidence: f64,
    pub capture_ms: u64,
    pub decision_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VncAssertOutcome {
    pub condition: String,
    pub passed: bool,
    pub probability: f64,
    pub confidence: f64,
    pub capture_ms: u64,
    pub decision_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VncChoiceOutcome {
    pub question: String,
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    pub capture_ms: u64,
    pub decision_ms: u64,
    pub total_ms: u64,
}

enum VncTransport {
    Ws(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
    Tcp(TcpStream),
}

/// Minimal RFB 3.8 client supporting both direct TCP (`vnc://host:port` or `host:port`) and
/// WebSocket (`ws://...` / `wss://...`) VNC endpoints with Security Type None (1).
pub struct VncClient {
    transport: VncTransport,
    buf: VecDeque<u8>,
    pub width: u16,
    pub height: u16,
    pub name: String,
    framebuffer: RgbImage,
}

impl VncClient {
    pub async fn connect(vnc_url: &str, host_id: Option<&str>) -> Result<Self> {
        let transport = if vnc_url.starts_with("ws://") || vnc_url.starts_with("wss://") {
            let ws_url = match host_id {
                Some(h) if !h.is_empty() && !vnc_url.contains("host=") => {
                    if vnc_url.contains('?') {
                        format!("{vnc_url}&host={h}")
                    } else {
                        format!("{vnc_url}?host={h}")
                    }
                }
                _ => vnc_url.to_string(),
            };
            let (ws, _) = tokio_tungstenite::connect_async(&ws_url)
                .await
                .map_err(|e| UicError::Browser(format!("VNC WebSocket connect failed ({ws_url}): {e}")))?;
            VncTransport::Ws(Box::new(ws))
        } else {
            let addr = vnc_url
                .strip_prefix("vnc://")
                .or_else(|| vnc_url.strip_prefix("tcp://"))
                .unwrap_or(vnc_url);
            let stream = TcpStream::connect(addr)
                .await
                .map_err(|e| UicError::Browser(format!("VNC TCP connect failed ({addr}): {e}")))?;
            VncTransport::Tcp(stream)
        };

        let mut client = Self {
            transport,
            buf: VecDeque::with_capacity(65536),
            width: 0,
            height: 0,
            name: String::new(),
            framebuffer: RgbImage::new(1, 1),
        };

        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<()> {
        // 1. ServerProtocolVersion (12 bytes) -> respond RFB 003.008\n
        let mut ver = [0u8; 12];
        self.read_exact(&mut ver).await?;
        if !ver.starts_with(b"RFB ") {
            return Err(UicError::Browser(format!(
                "invalid RFB server greeting: {:?}",
                String::from_utf8_lossy(&ver)
            )));
        }
        self.write_all(b"RFB 003.008\n").await?;

        // 2. Security types
        let count = self.read_u8().await?;
        if count == 0 {
            let reason_len = self.read_u32().await? as usize;
            let mut reason = vec![0u8; reason_len.min(1024)];
            self.read_exact(&mut reason).await?;
            return Err(UicError::Browser(format!(
                "VNC server refused connection: {}",
                String::from_utf8_lossy(&reason)
            )));
        }
        let mut types = vec![0u8; count as usize];
        self.read_exact(&mut types).await?;
        if !types.contains(&1) {
            return Err(UicError::Browser(format!(
                "VNC server does not offer Security Type None (1); offered: {types:?}"
            )));
        }
        self.write_all(&[1u8]).await?;

        // 3. SecurityResult
        let sec_res = self.read_u32().await?;
        if sec_res != 0 {
            return Err(UicError::Browser(format!(
                "VNC security handshake failed with code {sec_res}"
            )));
        }

        // 4. ClientInit (shared = 1)
        self.write_all(&[1u8]).await?;

        // 5. ServerInit
        self.width = self.read_u16().await?;
        self.height = self.read_u16().await?;
        let mut _pf = [0u8; 16];
        self.read_exact(&mut _pf).await?;
        let name_len = self.read_u32().await? as usize;
        let mut name_bytes = vec![0u8; name_len.min(4096)];
        self.read_exact(&mut name_bytes).await?;
        self.name = String::from_utf8_lossy(&name_bytes).to_string();
        self.framebuffer = RgbImage::new(self.width as u32, self.height as u32);

        // 6. SetPixelFormat: 32bpp, depth 24, little-endian, true-color, RGB at shifts 0,8,16
        let mut spf = [0u8; 20];
        spf[0] = 0; // SetPixelFormat
        spf[4] = 32; // bits-per-pixel
        spf[5] = 24; // depth
        spf[6] = 0; // big-endian = false
        spf[7] = 1; // true-color = true
        spf[8..10].copy_from_slice(&255u16.to_be_bytes()); // red-max
        spf[10..12].copy_from_slice(&255u16.to_be_bytes()); // green-max
        spf[12..14].copy_from_slice(&255u16.to_be_bytes()); // blue-max
        spf[14] = 0; // red-shift
        spf[15] = 8; // green-shift
        spf[16] = 16; // blue-shift
        self.write_all(&spf).await?;

        // 7. SetEncodings: Raw (0), CopyRect (1), DesktopSize (-223)
        let encodings: [i32; 3] = [0, 1, -223];
        let mut se = Vec::with_capacity(4 + encodings.len() * 4);
        se.push(2); // SetEncodings
        se.push(0);
        se.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
        for enc in encodings {
            se.extend_from_slice(&enc.to_be_bytes());
        }
        self.write_all(&se).await?;

        Ok(())
    }

    /// Requests a fresh full-screen framebuffer update and returns the RGB image.
    pub async fn capture_frame(&mut self) -> Result<RgbImage> {
        let mut req = [0u8; 10];
        req[0] = 3; // FramebufferUpdateRequest
        req[1] = 0; // incremental = false (full refresh)
        req[2..4].copy_from_slice(&0u16.to_be_bytes());
        req[4..6].copy_from_slice(&0u16.to_be_bytes());
        req[6..8].copy_from_slice(&self.width.to_be_bytes());
        req[8..10].copy_from_slice(&self.height.to_be_bytes());
        self.write_all(&req).await?;

        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let msg_type = self.read_u8().await?;
            match msg_type {
                0 => {
                    // FramebufferUpdate
                    let _pad = self.read_u8().await?;
                    let num_rects = self.read_u16().await?;
                    let mut got_pixels = false;
                    for _ in 0..num_rects {
                        let rx = self.read_u16().await? as u32;
                        let ry = self.read_u16().await? as u32;
                        let rw = self.read_u16().await? as u32;
                        let rh = self.read_u16().await? as u32;
                        let enc = self.read_i32().await?;
                        match enc {
                            0 => {
                                // Raw 32bpp RGBX (red=byte0, green=byte1, blue=byte2)
                                let byte_len = (rw as usize) * (rh as usize) * 4;
                                let mut raw = vec![0u8; byte_len];
                                self.read_exact(&mut raw).await?;
                                let fb_w = self.framebuffer.width();
                                let fb_h = self.framebuffer.height();
                                for dy in 0..rh {
                                    let py = ry + dy;
                                    if py >= fb_h {
                                        continue;
                                    }
                                    for dx in 0..rw {
                                        let px = rx + dx;
                                        if px >= fb_w {
                                            continue;
                                        }
                                        let off = ((dy * rw + dx) as usize) * 4;
                                        self.framebuffer.put_pixel(
                                            px,
                                            py,
                                            Rgb([raw[off], raw[off + 1], raw[off + 2]]),
                                        );
                                    }
                                }
                                got_pixels = true;
                            }
                            1 => {
                                // CopyRect
                                let src_x = self.read_u16().await? as u32;
                                let src_y = self.read_u16().await? as u32;
                                let copy = self.framebuffer.clone();
                                for dy in 0..rh {
                                    for dx in 0..rw {
                                        if src_x + dx < copy.width()
                                            && src_y + dy < copy.height()
                                            && rx + dx < self.framebuffer.width()
                                            && ry + dy < self.framebuffer.height()
                                        {
                                            let p = *copy.get_pixel(src_x + dx, src_y + dy);
                                            self.framebuffer.put_pixel(rx + dx, ry + dy, p);
                                        }
                                    }
                                }
                                got_pixels = true;
                            }
                            -223 => {
                                // DesktopSize pseudo-encoding
                                self.width = rw as u16;
                                self.height = rh as u16;
                                self.framebuffer = RgbImage::new(rw.max(1), rh.max(1));
                            }
                            other => {
                                return Err(UicError::Browser(format!(
                                    "unsupported VNC encoding {other}"
                                )));
                            }
                        }
                    }
                    if got_pixels {
                        return Ok(self.framebuffer.clone());
                    }
                }
                1 => {
                    // SetColourMapEntries
                    let _pad = self.read_u8().await?;
                    let _first = self.read_u16().await?;
                    let count = self.read_u16().await? as usize;
                    let mut skip = vec![0u8; count * 6];
                    self.read_exact(&mut skip).await?;
                }
                2 => {
                    // Bell (no payload)
                }
                3 => {
                    // ServerCutText
                    let mut _pad = [0u8; 3];
                    self.read_exact(&mut _pad).await?;
                    let len = self.read_u32().await? as usize;
                    let mut skip = vec![0u8; len];
                    self.read_exact(&mut skip).await?;
                }
                150 => {
                    // EndOfContinuousUpdates (0-byte payload)
                }
                248 => {
                    // Fence: 3 pad + 4 flags + 1 len + payload
                    let mut _hdr = [0u8; 7];
                    self.read_exact(&mut _hdr).await?;
                    let len = self.read_u8().await? as usize;
                    let mut skip = vec![0u8; len];
                    self.read_exact(&mut skip).await?;
                }
                other => {
                    return Err(UicError::Browser(format!(
                        "unexpected RFB server message type {other}"
                    )));
                }
            }
        }
        Err(UicError::Timeout("timed out waiting for VNC framebuffer".into()))
    }

    pub async fn pointer_event(&mut self, button_mask: u8, x: u16, y: u16) -> Result<()> {
        let mut msg = [0u8; 6];
        msg[0] = 5; // PointerEvent
        msg[1] = button_mask;
        msg[2..4].copy_from_slice(&x.to_be_bytes());
        msg[4..6].copy_from_slice(&y.to_be_bytes());
        self.write_all(&msg).await
    }

    pub async fn click_xy(&mut self, x: u16, y: u16) -> Result<()> {
        self.pointer_event(0, x, y).await?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        self.pointer_event(1, x, y).await?;
        tokio::time::sleep(Duration::from_millis(40)).await;
        self.pointer_event(0, x, y).await?;
        tokio::time::sleep(Duration::from_millis(60)).await;
        Ok(())
    }

    pub async fn key_event(&mut self, down: bool, keysym: u32) -> Result<()> {
        let mut msg = [0u8; 8];
        msg[0] = 4; // KeyEvent
        msg[1] = if down { 1 } else { 0 };
        msg[4..8].copy_from_slice(&keysym.to_be_bytes());
        self.write_all(&msg).await
    }

    pub async fn press_keysym(&mut self, keysym: u32) -> Result<()> {
        self.key_event(true, keysym).await?;
        tokio::time::sleep(Duration::from_millis(15)).await;
        self.key_event(false, keysym).await?;
        tokio::time::sleep(Duration::from_millis(15)).await;
        Ok(())
    }

    pub async fn type_text(&mut self, text: &str, submit: bool) -> Result<()> {
        for ch in text.chars() {
            let ks = ch as u32;
            self.press_keysym(ks).await?;
        }
        if submit {
            self.press_key_name("Enter").await?;
        }
        Ok(())
    }

    pub async fn press_key_name(&mut self, key: &str) -> Result<()> {
        let ks = match key {
            "Enter" | "Return" => 0xff0d,
            "Escape" | "Esc" => 0xff1b,
            "Tab" => 0xff09,
            "Backspace" => 0xff08,
            "Space" => 0x0020,
            "Up" => 0xff52,
            "Down" => 0xff54,
            "Left" => 0xff51,
            "Right" => 0xff53,
            other if other.chars().count() == 1 => other.chars().next().unwrap() as u32,
            other => {
                return Err(UicError::Config(format!("unsupported VNC key name: {other}")));
            }
        };
        self.press_keysym(ks).await
    }

    /// 2-stage coarse-to-fine visual grid click using Gemini Decisions API (`gemini-decision-1`).
    /// Stage 1: 8x6 labeled grid across the full desktop.
    /// Stage 2: 4x4 subgrid over the winning cell (zoomed crop) for precise button targeting.
    pub async fn click_target(
        &mut self,
        decisions: &DecisionsClient,
        target: &str,
        min_confidence: f64,
    ) -> Result<VncClickOutcome> {
        let total_t0 = Instant::now();
        let cap_t0 = Instant::now();
        let frame = self.capture_frame().await?;
        let capture_ms = cap_t0.elapsed().as_millis() as u64;

        let fw = frame.width();
        let fh = frame.height();

        // Downscale full frame to max 1280px wide for fast API upload while mapping coordinates back
        let stage1_img = if fw > 1280 {
            let nh = ((fh as f64) * (1280.0 / fw as f64)).round() as u32;
            imageops::resize(&frame, 1280, nh, imageops::FilterType::Triangle)
        } else {
            frame.clone()
        };

        let cols1 = 4usize;
        let rows1 = 3usize;
        let (jpeg1, cells1) = vision::draw_grid_overlay_jpeg(&stage1_img, cols1, rows1, 80)?;

        let mut criteria1 = BTreeMap::new();
        for (idx, (c, r)) in cells1.iter().enumerate() {
            let key = format!("c{}", idx + 1);
            criteria1.insert(
                key,
                format!("Region [{}] (column {} of {}, row {} of {})", idx + 1, c + 1, cols1, r + 1, rows1),
            );
        }
        criteria1.insert(
            "none".to_string(),
            "Completely absent: the target UI element is not visible anywhere on the screen.".to_string(),
        );

        let mut q1 = BTreeMap::new();
        q1.insert(
            "cell".to_string(),
            Question::Choice {
                instructions: format!(
                    "The desktop screenshot is divided into {} large numbered regions [1]..[{}] ({} columns x {} rows). Which numbered region contains: \"{target}\"? Only pick 'none' if it is truly not on the screen.",
                    cols1 * rows1,
                    cols1 * rows1,
                    cols1,
                    rows1
                ),
                criteria: criteria1,
            },
        );

        let d_t0 = Instant::now();
        let resp1 = decisions
            .decide(
                vec![
                    DecisionsClient::text_block(format!(
                        "VNC desktop '{}' ({}x{}). Locate: {target}",
                        self.name, fw, fh
                    )),
                    DecisionsClient::image_block_jpeg(&jpeg1),
                ],
                q1,
            )
            .await?;

        let (choice1, conf1, _) = resp1
            .answers
            .get("cell")
            .and_then(|a| a.as_choice())
            .ok_or_else(|| UicError::DecisionsApi("missing stage-1 'cell' answer".into()))?;

        if choice1 == "none" {
            return Err(UicError::TargetNotFound(target.to_string()));
        }
        if conf1 < min_confidence {
            return Err(UicError::LowConfidence {
                target: target.to_string(),
                confidence: conf1,
                threshold: min_confidence,
            });
        }

        let idx1: usize = choice1
            .strip_prefix('c')
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let (col1, row1) = cells1[idx1.min(cells1.len() - 1)];

        // Stage 2: Crop slightly padded region around the winning cell from the FULL-RES frame
        let cell_w = fw as f64 / cols1 as f64;
        let cell_h = fh as f64 / rows1 as f64;
        let pad_x = cell_w * 0.25;
        let pad_y = cell_h * 0.25;
        let crop_x0 = ((col1 as f64 * cell_w) - pad_x).max(0.0).floor() as u32;
        let crop_y0 = ((row1 as f64 * cell_h) - pad_y).max(0.0).floor() as u32;
        let crop_x1 = (((col1 + 1) as f64 * cell_w) + pad_x).min(fw as f64).ceil() as u32;
        let crop_y1 = (((row1 + 1) as f64 * cell_h) + pad_y).min(fh as f64).ceil() as u32;
        let crop_w = (crop_x1 - crop_x0).max(16);
        let crop_h = (crop_y1 - crop_y0).max(16);

        let cropped = imageops::crop_imm(&frame, crop_x0, crop_y0, crop_w, crop_h).to_image();
        // Upscale crop slightly if small so badges are super legible
        let zoom_w = crop_w.max(480);
        let zoom_h = ((crop_h as f64) * (zoom_w as f64 / crop_w as f64)).round() as u32;
        let zoomed = imageops::resize(&cropped, zoom_w, zoom_h, imageops::FilterType::Triangle);

        let cols2 = 5usize;
        let rows2 = 4usize;
        let (jpeg2, cells2) = vision::draw_grid_overlay_jpeg(&zoomed, cols2, rows2, 84)?;

        let mut criteria2 = BTreeMap::new();
        for (idx, (c, r)) in cells2.iter().enumerate() {
            criteria2.insert(
                format!("s{}", idx + 1),
                format!("Sub-cell [{}] (col {}, row {})", idx + 1, c + 1, r + 1),
            );
        }
        let mut q2 = BTreeMap::new();
        q2.insert(
            "subcell".to_string(),
            Question::Choice {
                instructions: format!(
                    "This is a zoomed-in crop around the target, divided into a {}x{} grid [1]..[{}]. Which sub-cell [N] is closest to the clickable center of: \"{target}\"?",
                    cols2,
                    rows2,
                    cols2 * rows2
                ),
                criteria: criteria2,
            },
        );

        let resp2 = decisions
            .decide(
                vec![
                    DecisionsClient::text_block(format!("Zoomed crop for target: {target}")),
                    DecisionsClient::image_block_jpeg(&jpeg2),
                ],
                q2,
            )
            .await?;
        let decision_ms = d_t0.elapsed().as_millis() as u64;

        let (choice2, conf2, _) = resp2
            .answers
            .get("subcell")
            .and_then(|a| a.as_choice())
            .ok_or_else(|| UicError::DecisionsApi("missing stage-2 'subcell' answer".into()))?;

        let idx2: usize = choice2
            .strip_prefix('s')
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let (col2, row2) = cells2[idx2.min(cells2.len() - 1)];

        let click_x = (crop_x0 as f64 + (col2 as f64 + 0.5) * (crop_w as f64 / cols2 as f64))
            .round()
            .clamp(0.0, (fw - 1) as f64) as u16;
        let click_y = (crop_y0 as f64 + (row2 as f64 + 0.5) * (crop_h as f64 / rows2 as f64))
            .round()
            .clamp(0.0, (fh - 1) as f64) as u16;

        self.click_xy(click_x, click_y).await?;

        Ok(VncClickOutcome {
            target: target.to_string(),
            x: click_x,
            y: click_y,
            coarse_cell: choice1.to_string(),
            coarse_confidence: conf1,
            fine_cell: choice2.to_string(),
            fine_confidence: conf2,
            capture_ms,
            decision_ms,
            total_ms: total_t0.elapsed().as_millis() as u64,
        })
    }

    pub async fn assert_condition(
        &mut self,
        decisions: &DecisionsClient,
        condition: &str,
    ) -> Result<VncAssertOutcome> {
        let total_t0 = Instant::now();
        let cap_t0 = Instant::now();
        let frame = self.capture_frame().await?;
        let capture_ms = cap_t0.elapsed().as_millis() as u64;

        let img = if frame.width() > 1280 {
            let nh = ((frame.height() as f64) * (1280.0 / frame.width() as f64)).round() as u32;
            imageops::resize(&frame, 1280, nh, imageops::FilterType::Triangle)
        } else {
            frame
        };
        let jpeg = vision::encode_rgb_jpeg(&img, 78)?;

        let mut q = BTreeMap::new();
        q.insert(
            "assertion".to_string(),
            Question::Noul {
                instructions: format!(
                    "Evaluate whether the following statement is currently TRUE on this desktop screenshot: {condition}"
                ),
            },
        );

        let d_t0 = Instant::now();
        let resp = decisions
            .decide(
                vec![
                    DecisionsClient::text_block(format!("VNC desktop '{}'", self.name)),
                    DecisionsClient::image_block_jpeg(&jpeg),
                ],
                q,
            )
            .await?;
        let decision_ms = d_t0.elapsed().as_millis() as u64;

        let (passed, probability, confidence) = resp
            .answers
            .get("assertion")
            .and_then(|a| a.as_noul())
            .ok_or_else(|| UicError::DecisionsApi("missing 'assertion' noul answer".into()))?;

        Ok(VncAssertOutcome {
            condition: condition.to_string(),
            passed,
            probability,
            confidence,
            capture_ms,
            decision_ms,
            total_ms: total_t0.elapsed().as_millis() as u64,
        })
    }

    pub async fn choose(
        &mut self,
        decisions: &DecisionsClient,
        question: &str,
        criteria: BTreeMap<String, String>,
    ) -> Result<VncChoiceOutcome> {
        let total_t0 = Instant::now();
        let cap_t0 = Instant::now();
        let frame = self.capture_frame().await?;
        let capture_ms = cap_t0.elapsed().as_millis() as u64;

        let img = if frame.width() > 1280 {
            let nh = ((frame.height() as f64) * (1280.0 / frame.width() as f64)).round() as u32;
            imageops::resize(&frame, 1280, nh, imageops::FilterType::Triangle)
        } else {
            frame
        };
        let jpeg = vision::encode_rgb_jpeg(&img, 78)?;

        let mut q = BTreeMap::new();
        q.insert(
            "choice".to_string(),
            Question::Choice {
                instructions: question.to_string(),
                criteria,
            },
        );

        let d_t0 = Instant::now();
        let resp = decisions
            .decide(
                vec![
                    DecisionsClient::text_block(format!("VNC desktop '{}'", self.name)),
                    DecisionsClient::image_block_jpeg(&jpeg),
                ],
                q,
            )
            .await?;
        let decision_ms = d_t0.elapsed().as_millis() as u64;

        let (choice, confidence, probs) = resp
            .answers
            .get("choice")
            .and_then(|a| a.as_choice())
            .ok_or_else(|| UicError::DecisionsApi("missing 'choice' answer".into()))?;

        Ok(VncChoiceOutcome {
            question: question.to_string(),
            choice: choice.to_string(),
            confidence,
            probabilities: probs.clone(),
            capture_ms,
            decision_ms,
            total_ms: total_t0.elapsed().as_millis() as u64,
        })
    }

    // --- Low-level byte stream helpers ---

    async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        match &mut self.transport {
            VncTransport::Ws(ws) => ws
                .send(Message::Binary(bytes.to_vec()))
                .await
                .map_err(|e| UicError::Browser(format!("VNC WebSocket send failed: {e}"))),
            VncTransport::Tcp(stream) => stream
                .write_all(bytes)
                .await
                .map_err(|e| UicError::Browser(format!("VNC TCP write failed: {e}"))),
        }
    }

    async fn fill_buf(&mut self, needed: usize) -> Result<()> {
        while self.buf.len() < needed {
            match &mut self.transport {
                VncTransport::Ws(ws) => {
                    let next = tokio::time::timeout(Duration::from_secs(15), ws.next())
                        .await
                        .map_err(|_| UicError::Timeout("VNC WebSocket read timed out".into()))?;
                    match next {
                        Some(Ok(Message::Binary(data))) => self.buf.extend(data),
                        Some(Ok(Message::Ping(p))) => {
                            let _ = ws.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            return Err(UicError::Browser("VNC WebSocket closed".into()));
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            return Err(UicError::Browser(format!("VNC WebSocket read error: {e}")));
                        }
                    }
                }
                VncTransport::Tcp(stream) => {
                    let mut tmp = [0u8; 8192];
                    let n = tokio::time::timeout(Duration::from_secs(15), stream.read(&mut tmp))
                        .await
                        .map_err(|_| UicError::Timeout("VNC TCP read timed out".into()))?
                        .map_err(|e| UicError::Browser(format!("VNC TCP read error: {e}")))?;
                    if n == 0 {
                        return Err(UicError::Browser("VNC TCP connection closed".into()));
                    }
                    self.buf.extend(&tmp[..n]);
                }
            }
        }
        Ok(())
    }

    async fn read_exact(&mut self, dst: &mut [u8]) -> Result<()> {
        self.fill_buf(dst.len()).await?;
        for b in dst.iter_mut() {
            *b = self.buf.pop_front().unwrap();
        }
        Ok(())
    }

    async fn read_u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b).await?;
        Ok(b[0])
    }

    async fn read_u16(&mut self) -> Result<u16> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b).await?;
        Ok(u16::from_be_bytes(b))
    }

    async fn read_u32(&mut self) -> Result<u32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b).await?;
        Ok(u32::from_be_bytes(b))
    }

    async fn read_i32(&mut self) -> Result<i32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b).await?;
        Ok(i32::from_be_bytes(b))
    }
}
