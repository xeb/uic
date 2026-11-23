//! Live E2E integration tests against the real Gemini Decisions API (`gemini-decision-1`).
//! Run with: `cargo test --test integration_live -- --ignored --nocapture`

mod common;

use common::TestFixtureServer;
use uic::{BrowserConfig, DecisionsClient, TaskConfig, UicSession};

async fn make_live_session() -> UicSession {
    let decisions = DecisionsClient::from_env()
        .expect("GEMINI_API_KEY must be set to run live Decisions API tests");
    let browser_cfg = BrowserConfig {
        headless: false,
        auto_xvfb: true,
        ..Default::default()
    };
    UicSession::start(browser_cfg, decisions, TaskConfig::default())
        .await
        .expect("start Chrome session")
}

#[tokio::test]
#[ignore]
async fn live_ui_drift_and_latency_benchmark() {
    let fixtures = TestFixtureServer::start().await;
    let session = make_live_session().await;
    let runner = session.runner();

    for (path, out_sel) in [("/drift_a", "#out"), ("/drift_b", "#banner")] {
        runner.open(&format!("{}{path}", fixtures.base_url)).await.unwrap();
        let t_out = runner
            .type_text("Username text input", "grace", true, false)
            .await
            .unwrap();
        let c_out = runner.click("Sign in / log in button").await.unwrap();
        println!(
            "[{path}] type={:?} (conf={:.2}) | click={:?} (conf={:.2})",
            t_out.timing, t_out.confidence, c_out.timing, c_out.confidence
        );

        let text = session
            .browser
            .cdp
            .evaluate(&format!("document.querySelector('{out_sel}').textContent"))
            .await
            .unwrap();
        assert_eq!(text.as_str().unwrap(), "Welcome grace");
    }
}

#[tokio::test]
#[ignore]
async fn live_modal_table_shadow_and_scroll() {
    let fixtures = TestFixtureServer::start().await;
    let session = make_live_session().await;
    let runner = session.runner();

    // 1. Modal auto-dismissal
    runner.open(&format!("{}/modal", fixtures.base_url)).await.unwrap();
    let m_out = runner.click("Download Report button").await.unwrap();
    println!("[modal] steps={} timing={:?}", m_out.steps_taken, m_out.timing);
    let msg = session
        .browser
        .cdp
        .evaluate("document.getElementById('msg').textContent")
        .await
        .unwrap();
    assert_eq!(msg.as_str().unwrap(), "Report Downloaded!");

    // 2. Table row disambiguation
    runner.open(&format!("{}/table", fixtures.base_url)).await.unwrap();
    let tbl_out = runner.click("Delete button for Bob Martinez").await.unwrap();
    println!("[table] conf={:.2} timing={:?}", tbl_out.confidence, tbl_out.timing);
    let log = session
        .browser
        .cdp
        .evaluate("document.getElementById('log').textContent")
        .await
        .unwrap();
    assert_eq!(log.as_str().unwrap(), "Deleted Bob");

    // 3. Delayed Shadow DOM
    runner
        .open(&format!("{}/spa_shadow", fixtures.base_url))
        .await
        .unwrap();
    let spa_out = runner.click("Launch Rocket button").await.unwrap();
    println!("[spa_shadow] steps={} timing={:?}", spa_out.steps_taken, spa_out.timing);
    let res = session
        .browser
        .cdp
        .evaluate("document.getElementById('result').textContent")
        .await
        .unwrap();
    assert_eq!(res.as_str().unwrap(), "Rocket Launched!");

    // 4. Below-the-fold scroll
    runner.open(&format!("{}/scroll", fixtures.base_url)).await.unwrap();
    let scr_out = runner.click("Confirm Order button").await.unwrap();
    println!("[scroll] steps={} timing={:?}", scr_out.steps_taken, scr_out.timing);
    let done = session
        .browser
        .cdp
        .evaluate("document.getElementById('done').textContent")
        .await
        .unwrap();
    assert_eq!(done.as_str().unwrap(), "Order Confirmed!");

    // 5. Visual/DOM assertion via noul
    let assertion = runner
        .assert_condition("The page displays 'Order Confirmed!'")
        .await
        .unwrap();
    println!(
        "[assert] passed={} p={:.3} timing={:?}",
        assertion.passed, assertion.probability, assertion.timing
    );
    assert!(assertion.passed);
}
