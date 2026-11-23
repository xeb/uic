mod common;

use common::{MockDecisionsServer, TestFixtureServer};
use uic::{BrowserConfig, DecisionsClient, Recipe, RecipeStep, TaskConfig, UicError, UicSession};

async fn make_session(mock_endpoint: &str) -> UicSession {
    let browser_cfg = BrowserConfig {
        headless: false, // exercises real non-headless Chrome (+ auto-Xvfb when DISPLAY is unset)
        auto_xvfb: true,
        ..Default::default()
    };
    let decisions = DecisionsClient::with_endpoint("test-key", mock_endpoint).unwrap();
    UicSession::start(browser_cfg, decisions, TaskConfig::default())
        .await
        .expect("start Chrome session")
}

#[tokio::test]
async fn test_basic_form_primitives() {
    let fixtures = TestFixtureServer::start().await;
    let mock = MockDecisionsServer::start().await;
    let session = make_session(&mock.endpoint).await;
    let runner = session.runner();

    runner.open(&format!("{}/basic", fixtures.base_url)).await.unwrap();
    runner
        .type_text("Work Email input", "alex@example.com", true, false)
        .await
        .unwrap();
    runner
        .select_option("Team Role dropdown", "Administrator")
        .await
        .unwrap();
    runner.click("Remember this device checkbox").await.unwrap();
    runner.click("Save Profile button").await.unwrap();

    let status = session
        .browser
        .cdp
        .evaluate("document.getElementById('status').textContent")
        .await
        .unwrap();
    assert_eq!(
        status.as_str().unwrap(),
        "Saved: alex@example.com (admin, remember=true)"
    );
}

#[tokio::test]
async fn test_ui_drift_resilience_across_variants() {
    let fixtures = TestFixtureServer::start().await;
    let mock = MockDecisionsServer::start().await;
    let session = make_session(&mock.endpoint).await;
    let runner = session.runner();

    // Same exact recipe runs unmodified against Variant A and Variant B
    for (path, out_selector) in [("/drift_a", "#out"), ("/drift_b", "#banner")] {
        let recipe = Recipe {
            name: Some(format!("login-{path}")),
            steps: vec![
                RecipeStep::Open {
                    url: format!("{}{path}", fixtures.base_url),
                },
                RecipeStep::Type {
                    target: "Username field".into(),
                    text: "ada".into(),
                    clear: true,
                    submit: false,
                },
                RecipeStep::Click {
                    target: "Sign In button".into(),
                },
            ],
        };
        let report = recipe.run(&runner).await.unwrap();
        assert_eq!(report.completed_steps, 3);

        let text = session
            .browser
            .cdp
            .evaluate(&format!("document.querySelector('{out_selector}').textContent"))
            .await
            .unwrap();
        assert_eq!(text.as_str().unwrap(), "Welcome ada");
    }
}

#[tokio::test]
async fn test_modal_auto_dismissal() {
    let fixtures = TestFixtureServer::start().await;
    let mock = MockDecisionsServer::start().await;
    let session = make_session(&mock.endpoint).await;
    let runner = session.runner();

    runner.open(&format!("{}/modal", fixtures.base_url)).await.unwrap();
    let out = runner.click("Download Report button").await.unwrap();
    assert!(out.steps_taken >= 2, "expected modal dismissal + target click");

    let msg = session
        .browser
        .cdp
        .evaluate("document.getElementById('msg').textContent")
        .await
        .unwrap();
    assert_eq!(msg.as_str().unwrap(), "Report Downloaded!");
}

#[tokio::test]
async fn test_spa_delayed_shadow_dom() {
    let fixtures = TestFixtureServer::start().await;
    let mock = MockDecisionsServer::start().await;
    let session = make_session(&mock.endpoint).await;
    let runner = session.runner();

    runner
        .open(&format!("{}/spa_shadow", fixtures.base_url))
        .await
        .unwrap();
    runner.click("Launch Rocket button").await.unwrap();

    let res = session
        .browser
        .cdp
        .evaluate("document.getElementById('result').textContent")
        .await
        .unwrap();
    assert_eq!(res.as_str().unwrap(), "Rocket Launched!");
}

#[tokio::test]
async fn test_below_the_fold_auto_scroll() {
    let fixtures = TestFixtureServer::start().await;
    let mock = MockDecisionsServer::start().await;
    let session = make_session(&mock.endpoint).await;
    let runner = session.runner();

    runner.open(&format!("{}/scroll", fixtures.base_url)).await.unwrap();
    let out = runner.click("Confirm Order button").await.unwrap();
    assert!(out.steps_taken >= 2, "expected scroll before clicking below-the-fold button");

    let done = session
        .browser
        .cdp
        .evaluate("document.getElementById('done').textContent")
        .await
        .unwrap();
    assert_eq!(done.as_str().unwrap(), "Order Confirmed!");
}

#[tokio::test]
async fn test_table_row_disambiguation() {
    let fixtures = TestFixtureServer::start().await;
    let mock = MockDecisionsServer::start().await;
    let session = make_session(&mock.endpoint).await;
    let runner = session.runner();

    runner.open(&format!("{}/table", fixtures.base_url)).await.unwrap();
    runner.click("Delete button for Bob Martinez").await.unwrap();

    let log = session
        .browser
        .cdp
        .evaluate("document.getElementById('log').textContent")
        .await
        .unwrap();
    assert_eq!(log.as_str().unwrap(), "Deleted Bob");
}

#[tokio::test]
async fn test_missing_element_fails_cleanly() {
    let fixtures = TestFixtureServer::start().await;
    let mock = MockDecisionsServer::start().await;
    let cfg = TaskConfig {
        max_steps: 2,
        ..Default::default()
    };

    let browser_cfg = BrowserConfig {
        headless: true,
        ..Default::default()
    };
    let decisions = DecisionsClient::with_endpoint("test-key", &mock.endpoint).unwrap();
    let session = UicSession::start(browser_cfg, decisions, cfg).await.unwrap();
    let runner = session.runner();

    runner.open(&format!("{}/basic", fixtures.base_url)).await.unwrap();
    let err = runner
        .click("Nonexistent Quantum Teleporter Button")
        .await
        .unwrap_err();
    assert!(
        matches!(err, UicError::TargetNotFound(_) | UicError::LowConfidence { .. }),
        "unexpected error variant: {err:?}"
    );
}
