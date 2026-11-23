use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::cdp::CdpClient;
use crate::decisions::{DecisionsClient, InputBlock, Question, MAX_CHOICES_PER_CALL};
use crate::dom::{self, ActionableElement, PageSnapshot};
use crate::error::{Result, UicError};
use crate::vision;

#[derive(Debug, Clone)]
pub struct TaskConfig {
    /// Minimum calibrated confidence required before acting.
    pub min_confidence: f64,
    /// Maximum observe-decide-act iterations per high-level task (for scroll / modal / retry).
    pub max_steps: usize,
    /// JPEG quality for initial screenshot (fast); escalated to 90 on low-confidence retry.
    pub jpeg_quality: u8,
    /// Automatically detect and dismiss blocking modals/banners.
    pub auto_dismiss_modals: bool,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.55,
            max_steps: 6,
            jpeg_quality: 72,
            auto_dismiss_modals: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepTiming {
    pub dom_ms: u64,
    pub screenshot_ms: u64,
    pub annotate_ms: u64,
    pub decision_ms: u64,
    pub act_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub action: String,
    pub chosen_id: Option<String>,
    pub chosen_label: Option<String>,
    pub confidence: f64,
    pub steps_taken: usize,
    pub timing: StepTiming,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertOutcome {
    pub condition: String,
    pub passed: bool,
    pub probability: f64,
    pub confidence: f64,
    pub timing: StepTiming,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceOutcome {
    pub question: String,
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    pub timing: StepTiming,
}

pub struct TaskRunner<'a> {
    pub cdp: &'a CdpClient,
    pub decisions: &'a DecisionsClient,
    pub config: TaskConfig,
}

impl<'a> TaskRunner<'a> {
    pub fn new(cdp: &'a CdpClient, decisions: &'a DecisionsClient, config: TaskConfig) -> Self {
        Self {
            cdp,
            decisions,
            config,
        }
    }

    pub async fn open(&self, url: &str) -> Result<TaskOutcome> {
        let t0 = Instant::now();
        self.cdp.navigate(url).await?;
        let elapsed = t0.elapsed().as_millis() as u64;
        Ok(TaskOutcome {
            action: format!("open:{url}"),
            chosen_id: None,
            chosen_label: None,
            confidence: 1.0,
            steps_taken: 1,
            timing: StepTiming {
                act_ms: elapsed,
                total_ms: elapsed,
                ..Default::default()
            },
        })
    }

    pub async fn click(&self, target_desc: &str) -> Result<TaskOutcome> {
        self.locate_and_act(target_desc, ActionKind::Click).await
    }

    pub async fn type_text(
        &self,
        target_desc: &str,
        text: &str,
        clear: bool,
        submit: bool,
    ) -> Result<TaskOutcome> {
        self.locate_and_act(
            target_desc,
            ActionKind::Type {
                text: text.to_string(),
                clear,
                submit,
            },
        )
        .await
    }

    pub async fn select_option(
        &self,
        target_desc: &str,
        option_desc: &str,
    ) -> Result<TaskOutcome> {
        self.locate_and_act(
            target_desc,
            ActionKind::Select {
                option: option_desc.to_string(),
            },
        )
        .await
    }

    pub async fn scroll(&self, delta_y: f64) -> Result<TaskOutcome> {
        let t0 = Instant::now();
        self.cdp.scroll(640.0, 450.0, delta_y).await?;
        let elapsed = t0.elapsed().as_millis() as u64;
        Ok(TaskOutcome {
            action: format!("scroll:{delta_y}"),
            chosen_id: None,
            chosen_label: None,
            confidence: 1.0,
            steps_taken: 1,
            timing: StepTiming {
                act_ms: elapsed,
                total_ms: elapsed,
                ..Default::default()
            },
        })
    }

    pub async fn assert_condition(&self, condition: &str) -> Result<AssertOutcome> {
        let total_t0 = Instant::now();
        let (snap, jpeg, dom_ms, screenshot_ms, annotate_ms) =
            self.observe(self.config.jpeg_quality, false).await?;

        let mut questions = BTreeMap::new();
        questions.insert(
            "assertion".to_string(),
            Question::Noul {
                instructions: format!(
                    "Evaluate whether the following statement is currently TRUE on this web page: {condition}"
                ),
            },
        );

        let input = build_multimodal_input(&snap, &jpeg, None);
        let d_t0 = Instant::now();
        let resp = self.decisions.decide(input, questions).await?;
        let decision_ms = d_t0.elapsed().as_millis() as u64;

        let ans = resp
            .answers
            .get("assertion")
            .and_then(|a| a.as_noul())
            .ok_or_else(|| UicError::DecisionsApi("missing 'assertion' noul answer".into()))?;

        let (passed, probability, confidence) = ans;
        let timing = StepTiming {
            dom_ms,
            screenshot_ms,
            annotate_ms,
            decision_ms,
            act_ms: 0,
            total_ms: total_t0.elapsed().as_millis() as u64,
        };

        Ok(AssertOutcome {
            condition: condition.to_string(),
            passed,
            probability,
            confidence,
            timing,
        })
    }

    pub async fn wait_for(
        &self,
        condition: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<AssertOutcome> {
        let deadline = Instant::now() + timeout;
        loop {
            let outcome = self.assert_condition(condition).await?;
            if outcome.passed && outcome.confidence >= self.config.min_confidence {
                return Ok(outcome);
            }
            if Instant::now() >= deadline {
                return Err(UicError::Timeout(format!(
                    "condition not met before timeout (last p={:.2}): {condition}",
                    outcome.probability
                )));
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    pub async fn choose(
        &self,
        question: &str,
        criteria: BTreeMap<String, String>,
    ) -> Result<ChoiceOutcome> {
        if criteria.len() < 2 {
            return Err(UicError::Config(
                "choose requires at least 2 options".into(),
            ));
        }
        let total_t0 = Instant::now();
        let (snap, jpeg, dom_ms, screenshot_ms, annotate_ms) =
            self.observe(self.config.jpeg_quality, false).await?;

        let mut questions = BTreeMap::new();
        questions.insert(
            "choice".to_string(),
            Question::Choice {
                instructions: question.to_string(),
                criteria,
            },
        );

        let input = build_multimodal_input(&snap, &jpeg, None);
        let d_t0 = Instant::now();
        let resp = self.decisions.decide(input, questions).await?;
        let decision_ms = d_t0.elapsed().as_millis() as u64;

        let (choice, confidence, probs) = resp
            .answers
            .get("choice")
            .and_then(|a| a.as_choice())
            .ok_or_else(|| UicError::DecisionsApi("missing 'choice' answer".into()))?;

        Ok(ChoiceOutcome {
            question: question.to_string(),
            choice: choice.to_string(),
            confidence,
            probabilities: probs.clone(),
            timing: StepTiming {
                dom_ms,
                screenshot_ms,
                annotate_ms,
                decision_ms,
                act_ms: 0,
                total_ms: total_t0.elapsed().as_millis() as u64,
            },
        })
    }

    /// Observe-Decide-Act loop with automatic modal dismissal, scrolling, and confidence retry.
    async fn locate_and_act(&self, target_desc: &str, action: ActionKind) -> Result<TaskOutcome> {
        let total_t0 = Instant::now();
        let mut timing = StepTiming::default();
        let mut quality = self.config.jpeg_quality;
        let mut dismissed_modal = false;

        for step in 1..=self.config.max_steps {
            let (snap, annotated_jpeg, dom_ms, screenshot_ms, annotate_ms) =
                self.observe(quality, true).await?;
            timing.dom_ms += dom_ms;
            timing.screenshot_ms += screenshot_ms;
            timing.annotate_ms += annotate_ms;

            if snap.elements.is_empty() && !snap.can_scroll_down && !snap.can_scroll_up {
                // Page may still be rendering asynchronously (e.g. SPA spinner)
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }

            let d_t0 = Instant::now();
            let (chosen_key, confidence, is_blocked) = self
                .select_target_with_tournament(&snap, &annotated_jpeg, target_desc, &action)
                .await?;
            timing.decision_ms += d_t0.elapsed().as_millis() as u64;

            if chosen_key == "scroll_down" && snap.can_scroll_down {
                let a_t0 = Instant::now();
                self.cdp
                    .scroll(
                        snap.viewport_width / 2.0,
                        snap.viewport_height / 2.0,
                        snap.viewport_height * 0.72,
                    )
                    .await?;
                timing.act_ms += a_t0.elapsed().as_millis() as u64;
                continue;
            }

            if chosen_key == "scroll_up" && snap.can_scroll_up {
                let a_t0 = Instant::now();
                self.cdp
                    .scroll(
                        snap.viewport_width / 2.0,
                        snap.viewport_height / 2.0,
                        -snap.viewport_height * 0.72,
                    )
                    .await?;
                timing.act_ms += a_t0.elapsed().as_millis() as u64;
                continue;
            }

            if chosen_key == "wait" {
                tokio::time::sleep(Duration::from_millis(350)).await;
                continue;
            }

            // If target wasn't found and a modal/banner is blocking the UI, try dismissing it once
            if (chosen_key == "none" || confidence < self.config.min_confidence)
                && is_blocked
                && self.config.auto_dismiss_modals
                && !dismissed_modal
            {
                dismissed_modal = true;
                if self.try_dismiss_blocker(&snap, &annotated_jpeg).await? {
                    tokio::time::sleep(Duration::from_millis(180)).await;
                    continue;
                }
            }

            if chosen_key == "none" {
                if step < self.config.max_steps && snap.can_scroll_down {
                    self.cdp
                        .scroll(
                            snap.viewport_width / 2.0,
                            snap.viewport_height / 2.0,
                            snap.viewport_height * 0.72,
                        )
                        .await?;
                    continue;
                }
                if step < self.config.max_steps {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    continue;
                }
                return Err(UicError::TargetNotFound(target_desc.to_string()));
            }

            if confidence < self.config.min_confidence {
                if quality < 90 && step < self.config.max_steps {
                    // Escalate screenshot fidelity and retry once
                    quality = 90;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
                return Err(UicError::LowConfidence {
                    target: target_desc.to_string(),
                    confidence,
                    threshold: self.config.min_confidence,
                });
            }

            let el = snap
                .elements
                .iter()
                .find(|e| e.id == chosen_key)
                .ok_or_else(|| UicError::TargetNotFound(format!("{chosen_key} ({target_desc})")))?;

            let a_t0 = Instant::now();
            self.execute_action(el, &action).await?;
            timing.act_ms += a_t0.elapsed().as_millis() as u64;
            timing.total_ms = total_t0.elapsed().as_millis() as u64;

            return Ok(TaskOutcome {
                action: action.name().to_string(),
                chosen_id: Some(el.id.clone()),
                chosen_label: Some(el.describe()),
                confidence,
                steps_taken: step,
                timing,
            });
        }

        Err(UicError::TargetNotFound(target_desc.to_string()))
    }

    async fn observe(&self, jpeg_quality: u8, annotate: bool) -> Result<(PageSnapshot, Vec<u8>, u64, u64, u64)> {
        let t_dom = Instant::now();
        let snap = dom::extract_snapshot(self.cdp).await?;
        let dom_ms = t_dom.elapsed().as_millis() as u64;

        let t_ss = Instant::now();
        let raw_jpeg = self.cdp.capture_screenshot_jpeg(jpeg_quality).await?;
        let screenshot_ms = t_ss.elapsed().as_millis() as u64;

        let t_ann = Instant::now();
        let jpeg = if annotate && !snap.elements.is_empty() {
            vision::annotate_screenshot_jpeg(
                &raw_jpeg,
                &snap.elements,
                snap.viewport_width,
                snap.viewport_height,
                jpeg_quality,
            )?
        } else {
            raw_jpeg
        };
        let annotate_ms = t_ann.elapsed().as_millis() as u64;

        Ok((snap, jpeg, dom_ms, screenshot_ms, annotate_ms))
    }

    async fn select_target_with_tournament(
        &self,
        snap: &PageSnapshot,
        annotated_jpeg: &[u8],
        target_desc: &str,
        action: &ActionKind,
    ) -> Result<(String, f64, bool)> {
        let elements: Vec<&ActionableElement> = if snap.elements.len() <= MAX_CHOICES_PER_CALL - 5 {
            snap.elements.iter().collect()
        } else {
            // 2-stage coarse tournament for ultra-dense pages (>245 actionable elements)
            let chunk_size = 200;
            let chunks: Vec<&[ActionableElement]> = snap.elements.chunks(chunk_size).collect();
            let mut group_criteria = BTreeMap::new();
            for (i, chunk) in chunks.iter().enumerate() {
                let first = chunk.first().map(|e| e.index).unwrap_or(0);
                let last = chunk.last().map(|e| e.index).unwrap_or(0);
                let sample: Vec<String> = chunk
                    .iter()
                    .take(8)
                    .map(|e| e.describe())
                    .collect();
                group_criteria.insert(
                    format!("g{i}"),
                    format!("Elements [{first}..{last}]: {}", sample.join("; ")),
                );
            }
            let mut q = BTreeMap::new();
            q.insert(
                "group".to_string(),
                Question::Choice {
                    instructions: format!(
                        "Which group contains the UI element for: {} ({target_desc})?",
                        action.verb_hint()
                    ),
                    criteria: group_criteria,
                },
            );
            let input = build_multimodal_input(snap, annotated_jpeg, None);
            let resp = self.decisions.decide(input, q).await?;
            let g_idx = resp
                .answers
                .get("group")
                .and_then(|a| a.as_choice())
                .and_then(|(k, _, _)| k.strip_prefix('g')?.parse::<usize>().ok())
                .unwrap_or(0);
            chunks[g_idx.min(chunks.len() - 1)].iter().collect()
        };

        let mut criteria = BTreeMap::new();
        for el in &elements {
            criteria.insert(el.id.clone(), el.describe());
        }
        if snap.can_scroll_down {
            criteria.insert(
                "scroll_down".to_string(),
                "The target element is not currently visible; scroll down to reveal more of the page.".to_string(),
            );
        }
        if snap.can_scroll_up {
            criteria.insert(
                "scroll_up".to_string(),
                "The target element is above the current viewport; scroll up.".to_string(),
            );
        }
        criteria.insert(
            "wait".to_string(),
            "The page is still loading or showing a spinner/skeleton; wait briefly.".to_string(),
        );
        criteria.insert(
            "none".to_string(),
            "No matching element exists on this page (or a blocking modal must be closed first).".to_string(),
        );

        let mut questions = BTreeMap::new();
        questions.insert(
            "target".to_string(),
            Question::Choice {
                instructions: format!(
                    "Select the single numbered UI element [N] to {} for the task: \"{target_desc}\". Match using both the annotated screenshot badges and the element descriptions (including row/section context). Choose scroll_down/scroll_up if the target is off-screen, wait if loading, or none if absent/blocked.",
                    action.verb_hint()
                ),
                criteria,
            },
        );
        questions.insert(
            "blocked".to_string(),
            Question::Noul {
                instructions: "Is a modal dialog, cookie consent banner, or popup overlay currently covering and blocking interaction with the main page?".to_string(),
            },
        );

        let input = build_multimodal_input(snap, annotated_jpeg, Some(&elements));
        let resp = self.decisions.decide(input, questions).await?;

        let (choice, confidence, _) = resp
            .answers
            .get("target")
            .and_then(|a| a.as_choice())
            .ok_or_else(|| UicError::DecisionsApi("missing 'target' choice answer".into()))?;

        let is_blocked = resp
            .answers
            .get("blocked")
            .and_then(|a| a.as_noul())
            .map(|(v, p, _)| v && p >= 0.65)
            .unwrap_or(false);

        Ok((choice.to_string(), confidence, is_blocked))
    }

    async fn try_dismiss_blocker(&self, snap: &PageSnapshot, annotated_jpeg: &[u8]) -> Result<bool> {
        let mut criteria = BTreeMap::new();
        for el in snap.elements.iter().take(MAX_CHOICES_PER_CALL - 2) {
            criteria.insert(el.id.clone(), el.describe());
        }
        criteria.insert(
            "none".to_string(),
            "No dismiss/close/accept button is available.".to_string(),
        );

        let mut questions = BTreeMap::new();
        questions.insert(
            "dismiss".to_string(),
            Question::Choice {
                instructions: "A modal, cookie banner, or popup is blocking the page. Select the button [N] that closes, dismisses, or accepts the overlay so we can interact with the main page.".to_string(),
                criteria,
            },
        );

        let input = build_multimodal_input(snap, annotated_jpeg, None);
        let resp = self.decisions.decide(input, questions).await?;
        let Some((choice, confidence, _)) = resp.answers.get("dismiss").and_then(|a| a.as_choice()) else {
            return Ok(false);
        };
        if choice == "none" || confidence < 0.50 {
            return Ok(false);
        }
        if let Some(el) = snap.elements.iter().find(|e| e.id == choice) {
            self.cdp.click_point(el.rect.cx, el.rect.cy).await?;
            return Ok(true);
        }
        Ok(false)
    }

    async fn execute_action(&self, el: &ActionableElement, action: &ActionKind) -> Result<()> {
        match action {
            ActionKind::Click => {
                self.cdp.click_point(el.rect.cx, el.rect.cy).await?;
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
            ActionKind::Type {
                text,
                clear,
                submit,
            } => {
                self.cdp.click_point(el.rect.cx, el.rect.cy).await?;
                let _ = dom::focus_and_prepare_input(self.cdp, &el.id, *clear).await;
                self.cdp.insert_text(text).await?;
                if *submit {
                    self.cdp.press_key("Enter").await?;
                }
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
            ActionKind::Select { option } => {
                if el.tag == "select" {
                    let matched = dom::select_native_option(self.cdp, &el.id, option).await?;
                    if !matched {
                        return Err(UicError::TargetNotFound(format!(
                            "option '{option}' not found in select '{}'",
                            el.label
                        )));
                    }
                } else {
                    // Custom dropdown: click to open, then pick option via Decisions API
                    self.cdp.click_point(el.rect.cx, el.rect.cy).await?;
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    let _ = Box::pin(self.locate_and_act(option, ActionKind::Click)).await?;
                }
            }
        }
        Ok(())
    }
}

enum ActionKind {
    Click,
    Type {
        text: String,
        clear: bool,
        submit: bool,
    },
    Select {
        option: String,
    },
}

impl ActionKind {
    fn name(&self) -> &'static str {
        match self {
            ActionKind::Click => "click",
            ActionKind::Type { .. } => "type",
            ActionKind::Select { .. } => "select",
        }
    }

    fn verb_hint(&self) -> &'static str {
        match self {
            ActionKind::Click => "click or activate",
            ActionKind::Type { .. } => "focus and type text into",
            ActionKind::Select { .. } => "select an option from",
        }
    }
}

fn build_multimodal_input(
    snap: &PageSnapshot,
    jpeg: &[u8],
    subset: Option<&[&ActionableElement]>,
) -> Vec<InputBlock> {
    let mut text = format!(
        "URL: {}\nTitle: {}\nViewport: {}x{} (scroll_y={}, can_scroll_down={}, can_scroll_up={})\nActionable elements:\n",
        snap.url,
        snap.title,
        snap.viewport_width as i64,
        snap.viewport_height as i64,
        snap.scroll_y as i64,
        snap.can_scroll_down,
        snap.can_scroll_up
    );
    match subset {
        Some(els) => {
            for el in els {
                text.push_str(&el.describe());
                text.push('\n');
            }
        }
        None => {
            for el in &snap.elements {
                text.push_str(&el.describe());
                text.push('\n');
            }
        }
    }

    vec![
        DecisionsClient::text_block(text),
        DecisionsClient::image_block_jpeg(jpeg),
    ]
}
