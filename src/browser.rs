use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::process::{Child, Command};

use crate::cdp::CdpClient;
use crate::error::{Result, UicError};

#[derive(Debug, Clone)]
pub struct BrowserConfig {
    /// Attach to an already-running Chrome instance (e.g. `http://127.0.0.1:9222`).
    pub cdp_url: Option<String>,
    /// Launch Chrome in headless mode instead of non-headless.
    pub headless: bool,
    /// When non-headless and DISPLAY/WAYLAND_DISPLAY are unset, auto-spawn Xvfb.
    pub auto_xvfb: bool,
    /// Explicit Google Chrome executable path (auto-detected if None).
    pub chrome_path: Option<PathBuf>,
    /// Load the user's default Google Chrome profile (preserves saved passwords, cookies, and sessions).
    pub use_default_profile: bool,
    /// Profile subdirectory inside user-data-dir (e.g. `"Default"`, `"Profile 1"`).
    pub profile_directory: Option<String>,
    /// Persistent user data directory (uses a fresh temporary profile if None and `use_default_profile` is false).
    pub user_data_dir: Option<PathBuf>,
    /// Initial viewport width and height.
    pub viewport: (u32, u32),
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            cdp_url: None,
            headless: false,
            auto_xvfb: true,
            chrome_path: None,
            use_default_profile: false,
            profile_directory: None,
            user_data_dir: None,
            viewport: (1280, 900),
        }
    }
}

pub struct BrowserHandle {
    pub cdp: CdpClient,
    pub cdp_http_url: String,
    chrome_child: Option<Child>,
    xvfb_child: Option<Child>,
    _temp_profile: Option<TempDir>,
}

impl Drop for BrowserHandle {
    fn drop(&mut self) {
        if let Some(mut child) = self.chrome_child.take() {
            let _ = child.start_kill();
        }
        if let Some(mut child) = self.xvfb_child.take() {
            let _ = child.start_kill();
        }
    }
}

#[derive(Debug, Deserialize)]
struct TargetInfo {
    #[serde(rename = "type")]
    target_type: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
    url: Option<String>,
}

impl BrowserHandle {
    pub async fn connect_or_launch(cfg: BrowserConfig) -> Result<Self> {
        if let Some(cdp_url) = cfg.cdp_url {
            let ws_url = resolve_page_ws_url(&cdp_url).await?;
            let cdp = CdpClient::connect(&ws_url).await?;
            return Ok(Self {
                cdp,
                cdp_http_url: cdp_url,
                chrome_child: None,
                xvfb_child: None,
                _temp_profile: None,
            });
        }

        let chrome_bin = find_chrome_binary(cfg.chrome_path.as_deref())?;
        let (profile_dir, temp_profile) = match (cfg.user_data_dir, cfg.use_default_profile) {
            (Some(dir), _) => (dir, None),
            (None, true) => {
                let default_dir = default_chrome_user_data_dir()?;
                // If Chrome is already running with remote debugging enabled on this profile, attach directly.
                if let Some(port) = read_active_devtools_port(&default_dir).await {
                    let cdp_http_url = format!("http://127.0.0.1:{port}");
                    if let Ok(ws_url) = resolve_page_ws_url(&cdp_http_url).await {
                        if let Ok(cdp) = CdpClient::connect(&ws_url).await {
                            return Ok(Self {
                                cdp,
                                cdp_http_url,
                                chrome_child: None,
                                xvfb_child: None,
                                _temp_profile: None,
                            });
                        }
                    }
                }
                if default_dir.join("SingletonLock").exists() {
                    return Err(UicError::Browser(format!(
                        "default Chrome profile ({}) is currently locked by a running Chrome instance without remote debugging; either close Chrome or start it with --remote-debugging-port=9222 and pass --cdp-url",
                        default_dir.display()
                    )));
                }
                (default_dir, None)
            }
            (None, false) => {
                let tmp = TempDir::new()?;
                let p = tmp.path().to_path_buf();
                (p, Some(tmp))
            }
        };

        let mut xvfb_child: Option<Child> = None;
        let mut display_override: Option<String> = None;

        if !cfg.headless && !has_active_display() {
            if cfg.auto_xvfb {
                let (display, child) = spawn_xvfb(cfg.viewport.0, cfg.viewport.1).await?;
                display_override = Some(display);
                xvfb_child = Some(child);
            } else {
                return Err(UicError::Browser(
                    "no DISPLAY/WAYLAND_DISPLAY set for non-headless Chrome (enable --auto-xvfb or pass --headless)".into(),
                ));
            }
        }

        let mut cmd = Command::new(&chrome_bin);
        cmd.arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile_dir.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-timer-throttling")
            .arg("--disable-renderer-backgrounding")
            .arg("--disable-ipc-flooding-protection")
            .arg("--disable-popup-blocking")
            .arg("--disable-gpu-sandbox")
            .arg("--no-sandbox")
            .arg(format!("--window-size={},{}", cfg.viewport.0, cfg.viewport.1))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        if let Some(prof) = &cfg.profile_directory {
            cmd.arg(format!("--profile-directory={prof}"));
        } else if cfg.use_default_profile {
            cmd.arg("--profile-directory=Default");
        }
        cmd.arg("about:blank");

        if cfg.headless {
            cmd.arg("--headless=new");
        }
        if let Some(disp) = &display_override {
            cmd.env("DISPLAY", disp);
        }

        let mut chrome_child = cmd.spawn().map_err(|e| {
            UicError::Browser(format!(
                "failed to launch Chrome ({}): {e}",
                chrome_bin.display()
            ))
        })?;

        let port = wait_for_devtools_port(&profile_dir, &mut chrome_child, Duration::from_secs(12))
            .await?;
        let cdp_http_url = format!("http://127.0.0.1:{port}");
        let ws_url = resolve_page_ws_url(&cdp_http_url).await?;
        let cdp = CdpClient::connect(&ws_url).await?;
        let _ = cdp.set_viewport(cfg.viewport.0, cfg.viewport.1).await;

        Ok(Self {
            cdp,
            cdp_http_url,
            chrome_child: Some(chrome_child),
            xvfb_child,
            _temp_profile: temp_profile,
        })
    }
}

fn has_active_display() -> bool {
    let x11 = std::env::var("DISPLAY").map(|s| !s.is_empty()).unwrap_or(false);
    let wayland = std::env::var("WAYLAND_DISPLAY")
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    x11 || wayland
}

fn find_chrome_binary(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(UicError::Browser(format!(
            "specified Chrome binary does not exist: {}",
            p.display()
        )));
    }
    if let Ok(env_path) = std::env::var("UIC_CHROME_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Ok(p);
        }
    }
    let candidates = [
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ];
    for c in candidates {
        let p = Path::new(c);
        if p.exists() {
            return Ok(p.to_path_buf());
        }
    }
    Err(UicError::Browser(
        "could not find Google Chrome binary; pass --chrome-path or set UIC_CHROME_PATH".into(),
    ))
}

/// Resolves the OS-standard Google Chrome user data directory (preserving saved passwords,
/// cookies, autofill, and active sessions via the system keyring).
pub fn default_chrome_user_data_dir() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME")
            .map_err(|_| UicError::Browser("HOME environment variable not set".into()))?;
        Ok(PathBuf::from(home).join("Library/Application Support/Google/Chrome"))
    } else if cfg!(target_os = "windows") {
        let local = std::env::var("LOCALAPPDATA")
            .map_err(|_| UicError::Browser("LOCALAPPDATA environment variable not set".into()))?;
        Ok(PathBuf::from(local).join("Google\\Chrome\\User Data"))
    } else {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map_err(|_| UicError::Browser("HOME/XDG_CONFIG_HOME not set".into()))?;
        Ok(base.join("google-chrome"))
    }
}

async fn read_active_devtools_port(profile_dir: &Path) -> Option<u16> {
    let contents = tokio::fs::read_to_string(profile_dir.join("DevToolsActivePort"))
        .await
        .ok()?;
    contents.lines().next()?.trim().parse::<u16>().ok()
}

static NEXT_XVFB_DISPLAY: AtomicU32 = AtomicU32::new(110);

async fn spawn_xvfb(width: u32, height: u32) -> Result<(String, Child)> {
    let screen = format!("{}x{}x24", width.max(1280), height.max(900));

    for _ in 0..80 {
        let n = NEXT_XVFB_DISPLAY.fetch_add(1, Ordering::Relaxed);
        let lock = format!("/tmp/.X{n}-lock");
        let sock = format!("/tmp/.X11-unix/X{n}");
        if Path::new(&lock).exists() || Path::new(&sock).exists() {
            continue;
        }
        let disp = format!(":{n}");
        let mut child = Command::new("Xvfb")
            .arg(&disp)
            .arg("-screen")
            .arg("0")
            .arg(&screen)
            .arg("-nolisten")
            .arg("tcp")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| UicError::Browser(format!("failed to spawn Xvfb: {e}")))?;

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                break; // Display collided with another process; try next display number
            }
            if Path::new(&sock).exists() {
                return Ok((disp, child));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let _ = child.start_kill();
    }

    Err(UicError::Browser(
        "could not allocate a free Xvfb display".into(),
    ))
}

async fn wait_for_devtools_port(
    profile_dir: &Path,
    child: &mut Child,
    timeout: Duration,
) -> Result<u16> {
    let port_file = profile_dir.join("DevToolsActivePort");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(UicError::Browser(format!(
                "Chrome exited prematurely with status {status}"
            )));
        }
        if let Ok(contents) = tokio::fs::read_to_string(&port_file).await {
            if let Some(first_line) = contents.lines().next() {
                if let Ok(port) = first_line.trim().parse::<u16>() {
                    return Ok(port);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    Err(UicError::Timeout(
        "timed out waiting for Chrome DevToolsActivePort".into(),
    ))
}

async fn resolve_page_ws_url(cdp_url: &str) -> Result<String> {
    if cdp_url.starts_with("ws://") || cdp_url.starts_with("wss://") {
        return Ok(cdp_url.to_string());
    }
    let base = cdp_url.trim_end_matches('/');
    let http = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| UicError::Browser(e.to_string()))?;

    let list_url = format!("{base}/json/list");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(resp) = http.get(&list_url).send().await {
            if let Ok(targets) = resp.json::<Vec<TargetInfo>>().await {
                // Prefer an active non-extension page target
                if let Some(t) = targets.iter().find(|t| {
                    t.target_type == "page"
                        && t.ws_url.is_some()
                        && !t
                            .url
                            .as_deref()
                            .unwrap_or("")
                            .starts_with("devtools://")
                }) {
                    return Ok(t.ws_url.clone().unwrap());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(UicError::Browser(format!(
                "no attachable Chrome page target found at {list_url}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
