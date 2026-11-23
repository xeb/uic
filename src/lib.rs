pub mod browser;
pub mod cdp;
pub mod decisions;
pub mod dom;
pub mod error;
pub mod recipe;
pub mod tasks;
pub mod vision;
pub mod vnc;

pub use browser::{BrowserConfig, BrowserHandle};
pub use cdp::CdpClient;
pub use decisions::DecisionsClient;
pub use error::{Result, UicError};
pub use recipe::{Recipe, RecipeReport, RecipeStep};
pub use tasks::{AssertOutcome, ChoiceOutcome, StepTiming, TaskConfig, TaskOutcome, TaskRunner};
pub use vnc::{VncAssertOutcome, VncChoiceOutcome, VncClickOutcome, VncClient};

/// High-level session bundling a Chrome browser handle and Gemini Decisions API client.
pub struct UicSession {
    pub browser: BrowserHandle,
    pub decisions: DecisionsClient,
    pub task_config: TaskConfig,
}

impl UicSession {
    pub async fn start(
        browser_cfg: BrowserConfig,
        decisions: DecisionsClient,
        task_config: TaskConfig,
    ) -> Result<Self> {
        let browser = BrowserHandle::connect_or_launch(browser_cfg).await?;
        Ok(Self {
            browser,
            decisions,
            task_config,
        })
    }

    pub fn runner(&self) -> TaskRunner<'_> {
        TaskRunner::new(&self.browser.cdp, &self.decisions, self.task_config.clone())
    }
}
