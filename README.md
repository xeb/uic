# `uic` — Fast, Resilient UI Control CLI

A minimal, pure-Rust CLI and library for controlling a local **Chrome** browser (via CDP) or remote desktop (via VNC/RFB 3.8) using the **Gemini Decisions API (`gemini-decision-1`)**.

## Why `gemini-decision-1`?
- **Speed (~150–250ms):** Single forward pass, **0 output tokens**, no JSON generation or prose parsing.
- **UI-Change Resilience:** Zero hardcoded CSS/XPath selectors. Every step extracts live visible actionable DOM + Shadow DOM + iframe elements, overlays Set-of-Mark badges `[1]..[N]` in-memory onto a JPEG screenshot (zero DOM mutation), and evaluates `target` (`choice`) + `blocked` (`noul`) in one request.
- **Calibrated Confidence:** Low-confidence decisions automatically escalate fidelity, scroll, dismiss blocking modals, or fail cleanly rather than mis-clicking.
- **Pure Rust Dependencies:** Zero native C/OpenSSL library dependencies, zero Node/Python runtimes — only standard crates from crates.io.

## Quickstart

```bash
export GEMINI_API_KEY="your-key"
cargo build --release

# Launch local non-headless Chrome (auto-falls back to Xvfb if DISPLAY is unset)
./target/release/uic --url https://example.com inspect --screenshot-out /tmp/annotated.jpg

# Launch using your default Google Chrome profile (saved passwords, cookies & sessions)
./target/release/uic --use-default-profile --url https://app.example.com click "Dashboard"

# Attach to an already-running Chrome started with --remote-debugging-port=9222
./target/release/uic --cdp-url http://127.0.0.1:9222 click "Sign In button"
./target/release/uic --cdp-url http://127.0.0.1:9222 type "Email input" "ada@example.com" --submit
./target/release/uic --cdp-url http://127.0.0.1:9222 assert "User dashboard is visible"

# Run a multi-step YAML/JSON recipe
./target/release/uic run workflow.yaml --json

# Control a remote desktop over VNC (direct TCP or WebSocket RFB 3.8)
./target/release/uic vnc --vnc-url vnc://127.0.0.1:5900 screenshot -o /tmp/desktop.jpg
./target/release/uic vnc --vnc-url vnc://127.0.0.1:5900 click "System Settings icon"
./target/release/uic vnc --vnc-url ws://127.0.0.1:6080/websockify assert "Terminal window is open"
```

### Example Recipe (`workflow.yaml`)
```yaml
name: login-flow
steps:
  - action: open
    url: https://app.example.com/login
  - action: type
    target: Work Email input
    text: ada@example.com
  - action: click
    target: Sign In button
  - action: assert
    condition: Welcome banner is visible
```

## Configuration

| Environment Variable | Description |
| --- | --- |
| `GEMINI_API_KEY` | API key for the Gemini Decisions API |
| `UIC_DECISIONS_URL` | Optional custom Decisions API endpoint |
| `UIC_CDP_URL` | Default Chrome DevTools endpoint (e.g. `http://127.0.0.1:9222`) |
| `UIC_CHROME_PATH` | Explicit path to the Chrome executable |
| `UIC_VNC_URL` | Default VNC endpoint (`vnc://host:port` or `ws://...`) |
| `UIC_VNC_HOST` | Optional `host` query parameter for WebSocket VNC proxies |

## Integration Tests

```bash
# Fast deterministic tests (real non-headless Chrome + CDP + DOM/Vision + Mock Decisions API)
cargo test

# Live E2E tests against real gemini-decision-1 (includes UI drift + latency breakdown)
cargo test --test integration_live -- --ignored --nocapture
```

## License

MIT
