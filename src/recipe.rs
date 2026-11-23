use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Result, UicError};
use crate::tasks::TaskRunner;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    #[serde(default)]
    pub name: Option<String>,
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RecipeStep {
    Open {
        url: String,
    },
    Click {
        target: String,
    },
    Type {
        target: String,
        text: String,
        #[serde(default = "default_true")]
        clear: bool,
        #[serde(default)]
        submit: bool,
    },
    Select {
        target: String,
        option: String,
    },
    Scroll {
        delta_y: f64,
    },
    Assert {
        condition: String,
    },
    WaitFor {
        condition: String,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
}

fn default_true() -> bool {
    true
}

fn default_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeReport {
    pub name: Option<String>,
    pub completed_steps: usize,
    pub total_steps: usize,
    pub step_results: Vec<serde_json::Value>,
}

impl Recipe {
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| e.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
        {
            serde_json::from_str(&raw).map_err(|e| UicError::Config(format!("invalid JSON recipe: {e}")))
        } else {
            serde_yaml::from_str(&raw).map_err(|e| UicError::Config(format!("invalid YAML recipe: {e}")))
        }
    }

    pub async fn run(&self, runner: &TaskRunner<'_>) -> Result<RecipeReport> {
        let mut results = Vec::with_capacity(self.steps.len());

        for step in &self.steps {
            let val = match step {
                RecipeStep::Open { url } => serde_json::to_value(runner.open(url).await?),
                RecipeStep::Click { target } => serde_json::to_value(runner.click(target).await?),
                RecipeStep::Type {
                    target,
                    text,
                    clear,
                    submit,
                } => serde_json::to_value(runner.type_text(target, text, *clear, *submit).await?),
                RecipeStep::Select { target, option } => {
                    serde_json::to_value(runner.select_option(target, option).await?)
                }
                RecipeStep::Scroll { delta_y } => serde_json::to_value(runner.scroll(*delta_y).await?),
                RecipeStep::Assert { condition } => {
                    let out = runner.assert_condition(condition).await?;
                    if !out.passed {
                        return Err(UicError::AssertionFailed {
                            condition: condition.clone(),
                            probability: out.probability,
                        });
                    }
                    serde_json::to_value(out)
                }
                RecipeStep::WaitFor {
                    condition,
                    timeout_ms,
                } => serde_json::to_value(
                    runner
                        .wait_for(
                            condition,
                            Duration::from_millis(*timeout_ms),
                            Duration::from_millis(250),
                        )
                        .await?,
                ),
            }
            .map_err(|e| UicError::Config(e.to_string()))?;

            results.push(val);
        }

        Ok(RecipeReport {
            name: self.name.clone(),
            completed_steps: results.len(),
            total_steps: self.steps.len(),
            step_results: results,
        })
    }
}
