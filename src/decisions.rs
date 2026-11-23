use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::{Result, UicError};

pub const DEFAULT_DECISIONS_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/decisions";
pub const DEFAULT_MODEL: &str = "gemini-decision-1";
pub const MAX_CHOICES_PER_CALL: usize = 250;

#[derive(Debug, Clone)]
pub struct DecisionsClient {
    http: Client,
    api_key: String,
    endpoint: String,
    model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputBlock {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        data: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    Noul {
        instructions: String,
    },
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub model: String,
    pub input: Vec<InputBlock>,
    pub questions: BTreeMap<String, Question>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionResponse {
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Noul {
        #[serde(default)]
        value: bool,
        #[serde(default)]
        noul: f64,
        #[serde(default)]
        true_probability: Option<f64>,
        #[serde(default)]
        false_probability: Option<f64>,
        #[serde(default)]
        confidence: f64,
        #[serde(default)]
        applicability: Option<f64>,
    },
    Choice {
        choice: String,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
        #[serde(default)]
        confidence: f64,
        #[serde(default)]
        applicability: Option<f64>,
    },
}

impl Answer {
    pub fn as_noul(&self) -> Option<(bool, f64, f64)> {
        match self {
            Answer::Noul {
                value,
                noul,
                true_probability,
                confidence,
                ..
            } => {
                let p = true_probability.unwrap_or(*noul);
                Some((*value, p, *confidence))
            }
            _ => None,
        }
    }

    pub fn as_choice(&self) -> Option<(&str, f64, &BTreeMap<String, f64>)> {
        match self {
            Answer::Choice {
                choice,
                probabilities,
                confidence,
                ..
            } => Some((choice.as_str(), *confidence, probabilities)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
}

impl DecisionsClient {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let endpoint = std::env::var("UIC_DECISIONS_URL")
            .unwrap_or_else(|_| DEFAULT_DECISIONS_URL.to_string());
        Self::with_endpoint(api_key, endpoint)
    }

    pub fn with_endpoint(
        api_key: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Result<Self> {
        let http = Client::builder()
            .pool_max_idle_per_host(4)
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| UicError::DecisionsApi(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            http,
            api_key: api_key.into(),
            endpoint: endpoint.into(),
            model: DEFAULT_MODEL.to_string(),
        })
    }

    pub fn from_env() -> Result<Self> {
        let endpoint = std::env::var("UIC_DECISIONS_URL")
            .unwrap_or_else(|_| DEFAULT_DECISIONS_URL.to_string());
        let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
        if api_key.is_empty() && endpoint == DEFAULT_DECISIONS_URL {
            return Err(UicError::Config(
                "GEMINI_API_KEY environment variable is not set".into(),
            ));
        }
        Self::with_endpoint(api_key, endpoint)
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn image_block_jpeg(jpeg_bytes: &[u8]) -> InputBlock {
        InputBlock::Image {
            mime_type: "image/jpeg".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(jpeg_bytes),
        }
    }

    pub fn text_block(text: impl Into<String>) -> InputBlock {
        InputBlock::Text { text: text.into() }
    }

    pub async fn decide(
        &self,
        input: Vec<InputBlock>,
        questions: BTreeMap<String, Question>,
    ) -> Result<DecisionResponse> {
        let body = DecisionRequest {
            model: self.model.clone(),
            input,
            questions,
        };

        let mut req = self.http.post(&self.endpoint).json(&body);
        if !self.api_key.is_empty() {
            req = req.header("x-goog-api-key", &self.api_key);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| UicError::DecisionsApi(format!("HTTP request failed: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UicError::DecisionsApi(format!("failed reading response body: {e}")))?;

        if !status.is_success() {
            return Err(UicError::DecisionsApi(format!(
                "HTTP {status}: {text}"
            )));
        }

        serde_json::from_str::<DecisionResponse>(&text).map_err(|e| {
            UicError::DecisionsApi(format!("failed to parse response JSON ({e}): {text}"))
        })
    }
}
