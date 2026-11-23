use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

use uic::{
    dom, vision, vnc, BrowserConfig, DecisionsClient, Recipe, TaskConfig, UicError, UicSession,
    VncClient,
};

#[derive(Debug, Parser)]
#[command(
    name = "uic",
    version,
    about = "Fast, UI-change-resilient browser control CLI powered by Chrome CDP + Gemini Decisions API"
)]
struct Cli {
    /// Attach to an existing Chrome DevTools endpoint (e.g. http://127.0.0.1:9222)
    #[arg(long, env = "UIC_CDP_URL", global = true)]
    cdp_url: Option<String>,

    /// Run Chrome in headless mode (default: non-headless, with automatic Xvfb if no display)
    #[arg(long, global = true)]
    headless: bool,

    /// Disable automatic Xvfb fallback when no DISPLAY/WAYLAND_DISPLAY is set
    #[arg(long, global = true)]
    no_auto_xvfb: bool,

    /// Path to Google Chrome binary
    #[arg(long, env = "UIC_CHROME_PATH", global = true)]
    chrome_path: Option<PathBuf>,

    /// Load the user's default Google Chrome profile (saved passwords, cookies, and sessions)
    #[arg(long, global = true)]
    use_default_profile: bool,

    /// Chrome profile directory name (e.g. "Default", "Profile 1")
    #[arg(long, global = true)]
    profile_directory: Option<String>,

    /// Persistent Chrome user-data-dir (default: fresh temporary profile)
    #[arg(long, global = true)]
    user_data_dir: Option<PathBuf>,

    /// Navigate to this URL before executing the command
    #[arg(long, global = true)]
    url: Option<String>,

    /// Minimum calibrated confidence required before acting (0.0 - 1.0)
    #[arg(long, default_value_t = 0.55, global = true)]
    min_confidence: f64,

    /// Output structured JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Navigate to a URL
    Open { url: String },

    /// Click an actionable UI element matching the description
    Click { target: String },

    /// Focus an input matching the description and type text
    Type {
        target: String,
        /// Text to type (omit if using --text-stdin to avoid exposing secrets in argv)
        text: Option<String>,
        /// Read text to type from stdin (trimmed of trailing newline; safe for passwords/tokens)
        #[arg(long)]
        text_stdin: bool,
        /// Do not clear existing input text before typing
        #[arg(long)]
        no_clear: bool,
        /// Press Enter after typing
        #[arg(long)]
        submit: bool,
    },

    /// Select an option from a native or custom dropdown
    Select { target: String, option: String },

    /// Scroll the page vertically (positive = down, negative = up)
    Scroll {
        #[arg(default_value_t = 600.0)]
        delta_y: f64,
    },

    /// Assert a visual/DOM condition using a calibrated Decisions API noul check
    Assert { condition: String },

    /// Poll until a condition becomes true
    WaitFor {
        condition: String,
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = 250)]
        poll_ms: u64,
    },

    /// Classify the current page state into one of the provided options (key=description)
    Choose {
        question: String,
        #[arg(short = 'o', long = "option", required = true)]
        options: Vec<String>,
    },

    /// Execute a multi-step YAML or JSON recipe file
    Run { recipe: PathBuf },

    /// Inspect extracted actionable elements (and optionally save the annotated screenshot)
    Inspect {
        #[arg(long)]
        screenshot_out: Option<PathBuf>,
    },

    /// Control a remote desktop over VNC (RFB 3.8 over TCP or WebSocket)
    Vnc {
        /// VNC endpoint URL (e.g. vnc://127.0.0.1:5900 or ws://127.0.0.1:6080/websockify)
        #[arg(long, env = "UIC_VNC_URL", default_value = vnc::DEFAULT_VNC_URL)]
        vnc_url: String,

        /// Optional host query parameter for WebSocket VNC proxies (appended as ?host=<HOST>)
        #[arg(long, env = "UIC_VNC_HOST")]
        host: Option<String>,

        #[command(subcommand)]
        action: VncCommands,
    },
}

#[derive(Debug, Subcommand)]
enum VncCommands {
    /// Capture a JPEG screenshot of the remote desktop
    Screenshot {
        #[arg(short, long)]
        out: PathBuf,
    },

    /// Visually locate and click a target using Gemini Decisions API (2-stage coarse-to-fine grid)
    Click { target: String },

    /// Click an exact (x, y) pixel coordinate
    ClickXy { x: u16, y: u16 },

    /// Type text into the remote desktop
    Type {
        text: Option<String>,
        #[arg(long)]
        text_stdin: bool,
        #[arg(long)]
        submit: bool,
    },

    /// Send a single key (Enter, Escape, Tab, Backspace, Space, Up, Down, Left, Right)
    Key { key: String },

    /// Assert a visual condition on the VNC desktop using Gemini Decisions API
    Assert { condition: String },

    /// Classify the VNC desktop state into one of the provided options (key=description)
    Choose {
        question: String,
        #[arg(short = 'o', long = "option", required = true)]
        options: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(err) = run_cli(cli).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run_cli(cli: Cli) -> uic::Result<()> {
    // Handle VNC mode directly without launching Chrome
    if let Commands::Vnc {
        vnc_url,
        host,
        action,
    } = cli.command
    {
        return run_vnc_cli(&vnc_url, host.as_deref(), action, cli.min_confidence, cli.json).await;
    }

    let browser_cfg = BrowserConfig {
        cdp_url: cli.cdp_url,
        headless: cli.headless,
        auto_xvfb: !cli.no_auto_xvfb,
        chrome_path: cli.chrome_path,
        use_default_profile: cli.use_default_profile,
        profile_directory: cli.profile_directory,
        user_data_dir: cli.user_data_dir,
        viewport: (1280, 900),
    };

    let task_cfg = TaskConfig {
        min_confidence: cli.min_confidence,
        ..Default::default()
    };

    // Inspect does not require GEMINI_API_KEY
    let decisions = match cli.command {
        Commands::Inspect { .. } | Commands::Open { .. } | Commands::Scroll { .. } => {
            DecisionsClient::from_env()
                .unwrap_or_else(|_| DecisionsClient::new("").expect("http client"))
        }
        _ => DecisionsClient::from_env()?,
    };

    let session = UicSession::start(browser_cfg, decisions, task_cfg).await?;
    let runner = session.runner();

    if let Some(url) = &cli.url {
        runner.open(url).await?;
    }

    match cli.command {
        Commands::Open { url } => {
            let out = runner.open(&url).await?;
            print_result(&out, cli.json);
        }
        Commands::Click { target } => {
            let out = runner.click(&target).await?;
            print_result(&out, cli.json);
        }
        Commands::Type {
            target,
            text,
            text_stdin,
            no_clear,
            submit,
        } => {
            let value = if text_stdin {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf.trim_end_matches(&['\r', '\n'][..]).to_string()
            } else {
                text.ok_or_else(|| {
                    UicError::Config("provide <TEXT> or pass --text-stdin".into())
                })?
            };
            let out = runner.type_text(&target, &value, !no_clear, submit).await?;
            print_result(&out, cli.json);
        }
        Commands::Select { target, option } => {
            let out = runner.select_option(&target, &option).await?;
            print_result(&out, cli.json);
        }
        Commands::Scroll { delta_y } => {
            let out = runner.scroll(delta_y).await?;
            print_result(&out, cli.json);
        }
        Commands::Assert { condition } => {
            let out = runner.assert_condition(&condition).await?;
            print_result(&out, cli.json);
            if !out.passed {
                return Err(UicError::AssertionFailed {
                    condition: out.condition,
                    probability: out.probability,
                });
            }
        }
        Commands::WaitFor {
            condition,
            timeout_ms,
            poll_ms,
        } => {
            let out = runner
                .wait_for(
                    &condition,
                    Duration::from_millis(timeout_ms),
                    Duration::from_millis(poll_ms),
                )
                .await?;
            print_result(&out, cli.json);
        }
        Commands::Choose { question, options } => {
            let mut criteria = BTreeMap::new();
            for opt in options {
                if let Some((k, v)) = opt.split_once('=') {
                    criteria.insert(k.trim().to_string(), v.trim().to_string());
                } else {
                    criteria.insert(opt.trim().to_string(), opt.trim().to_string());
                }
            }
            let out = runner.choose(&question, criteria).await?;
            print_result(&out, cli.json);
        }
        Commands::Run { recipe } => {
            let rec = Recipe::from_file(&recipe)?;
            let report = rec.run(&runner).await?;
            print_result(&report, cli.json);
        }
        Commands::Inspect { screenshot_out } => {
            let snap = dom::extract_snapshot(&session.browser.cdp).await?;
            if let Some(out_path) = screenshot_out {
                let raw = session.browser.cdp.capture_screenshot_jpeg(80).await?;
                let ann = vision::annotate_screenshot_jpeg(
                    &raw,
                    &snap.elements,
                    snap.viewport_width,
                    snap.viewport_height,
                    80,
                )?;
                std::fs::write(&out_path, ann)?;
            }
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&snap).unwrap());
            } else {
                println!("URL: {} ({} actionable elements)", snap.url, snap.elements.len());
                for el in &snap.elements {
                    println!("  {}", el.describe());
                }
            }
        }
        Commands::Vnc { .. } => unreachable!(),
    }

    Ok(())
}

async fn run_vnc_cli(
    vnc_url: &str,
    host: Option<&str>,
    action: VncCommands,
    min_confidence: f64,
    as_json: bool,
) -> uic::Result<()> {
    let mut client = VncClient::connect(vnc_url, host).await?;
    match action {
        VncCommands::Screenshot { out } => {
            let frame = client.capture_frame().await?;
            let jpeg = vision::encode_rgb_jpeg(&frame, 85)?;
            std::fs::write(&out, jpeg)?;
            let summary = serde_json::json!({
                "url": vnc_url,
                "host": host,
                "name": client.name,
                "width": client.width,
                "height": client.height,
                "saved_to": out.display().to_string(),
            });
            print_result(&summary, as_json);
        }
        VncCommands::Click { target } => {
            let decisions = DecisionsClient::from_env()?;
            let out = client
                .click_target(&decisions, &target, min_confidence)
                .await?;
            print_result(&out, as_json);
        }
        VncCommands::ClickXy { x, y } => {
            client.click_xy(x, y).await?;
            print_result(&serde_json::json!({ "action": "click_xy", "x": x, "y": y }), as_json);
        }
        VncCommands::Type {
            text,
            text_stdin,
            submit,
        } => {
            let value = if text_stdin {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf.trim_end_matches(&['\r', '\n'][..]).to_string()
            } else {
                text.ok_or_else(|| UicError::Config("provide <TEXT> or pass --text-stdin".into()))?
            };
            client.type_text(&value, submit).await?;
            print_result(&serde_json::json!({ "action": "type", "submit": submit }), as_json);
        }
        VncCommands::Key { key } => {
            client.press_key_name(&key).await?;
            print_result(&serde_json::json!({ "action": "key", "key": key }), as_json);
        }
        VncCommands::Assert { condition } => {
            let decisions = DecisionsClient::from_env()?;
            let out = client.assert_condition(&decisions, &condition).await?;
            print_result(&out, as_json);
            if !out.passed {
                return Err(UicError::AssertionFailed {
                    condition: out.condition,
                    probability: out.probability,
                });
            }
        }
        VncCommands::Choose { question, options } => {
            let decisions = DecisionsClient::from_env()?;
            let mut criteria = BTreeMap::new();
            for opt in options {
                if let Some((k, v)) = opt.split_once('=') {
                    criteria.insert(k.trim().to_string(), v.trim().to_string());
                } else {
                    criteria.insert(opt.trim().to_string(), opt.trim().to_string());
                }
            }
            let out = client.choose(&decisions, &question, criteria).await?;
            print_result(&out, as_json);
        }
    }
    Ok(())
}

fn print_result<T: serde::Serialize>(val: &T, as_json: bool) {
    if as_json {
        println!("{}", serde_json::to_string_pretty(val).unwrap());
    } else {
        println!("{}", serde_json::to_string(val).unwrap());
    }
}
