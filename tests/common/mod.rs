#![allow(dead_code)]

pub mod fixtures;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use uic::decisions::{Answer, DecisionRequest, DecisionResponse, InputBlock, Question, Usage};

pub struct TestFixtureServer {
    pub base_url: String,
    _handle: tokio::task::JoinHandle<()>,
}

impl TestFixtureServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");

                    let (status, body) = match fixtures::html_for_path(path) {
                        Some(html) => ("200 OK", html),
                        None => ("404 Not Found", "<h1>Not Found</h1>"),
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        Self {
            base_url,
            _handle: handle,
        }
    }
}

pub struct MockDecisionsServer {
    pub endpoint: String,
    pub calls: Arc<AtomicUsize>,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockDecisionsServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}/v1beta/decisions", addr.port());
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let calls = Arc::clone(&calls_clone);
                tokio::spawn(async move {
                    let Some(body) = read_http_body(&mut stream).await else {
                        return;
                    };
                    calls.fetch_add(1, Ordering::Relaxed);
                    let req: DecisionRequest = match serde_json::from_slice(&body) {
                        Ok(r) => r,
                        Err(e) => {
                            let err = format!(r#"{{"error":"{e}"}}"#);
                            let resp = format!(
                                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{err}",
                                err.len()
                            );
                            let _ = stream.write_all(resp.as_bytes()).await;
                            return;
                        }
                    };

                    // Verify multimodal image block is always included
                    let has_image = req
                        .input
                        .iter()
                        .any(|b| matches!(b, InputBlock::Image { data, .. } if !data.is_empty()));
                    assert!(has_image, "Decisions request must include a non-empty image block");

                    let context_text: String = req
                        .input
                        .iter()
                        .filter_map(|b| match b {
                            InputBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        .to_lowercase();

                    let resp_obj = evaluate_mock(&context_text, &req.questions);
                    let json = serde_json::to_string(&resp_obj).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        Self {
            endpoint,
            calls,
            _handle: handle,
        }
    }
}

async fn read_http_body(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];
    let mut header_end = None;
    let mut content_len = 0usize;

    loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if header_end.is_none() {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let headers = String::from_utf8_lossy(&buf[..pos]);
                for line in headers.lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
            }
        }
        if let Some(h_end) = header_end {
            if buf.len() >= h_end + content_len {
                return Some(buf[h_end..h_end + content_len].to_vec());
            }
        }
    }
    None
}

fn evaluate_mock(context_text: &str, questions: &BTreeMap<String, Question>) -> DecisionResponse {
    let mut answers = BTreeMap::new();

    for (qkey, q) in questions {
        match q {
            Question::Noul { instructions } => {
                let instr = instructions.to_lowercase();
                let is_true = if qkey == "blocked" {
                    context_text.contains("accept all cookies")
                        || context_text.contains("cookie preferences")
                } else {
                    // Check if quoted/key words from condition appear in page context
                    let tokens = extract_keywords(&instr);
                    let hits = tokens.iter().filter(|t| context_text.contains(t.as_str())).count();
                    hits >= tokens.len().clamp(1, 2)
                };
                let p = if is_true { 0.94 } else { 0.06 };
                answers.insert(
                    qkey.clone(),
                    Answer::Noul {
                        value: is_true,
                        noul: p,
                        true_probability: Some(p),
                        false_probability: Some(1.0 - p),
                        confidence: 0.94,
                        applicability: Some(1.0),
                    },
                );
            }
            Question::Choice {
                instructions,
                criteria,
            } => {
                let instr = instructions.to_lowercase();
                let target_phrase = extract_quoted(&instr).unwrap_or_else(|| instr.clone());
                let target_tokens = expand_synonyms(&extract_keywords(&target_phrase));

                // Score real element keys (e1..eN)
                let mut best_key = "none".to_string();
                let mut best_score = 0usize;

                for (k, desc) in criteria {
                    if matches!(k.as_str(), "scroll_down" | "scroll_up" | "wait" | "none") {
                        continue;
                    }
                    let d = desc.to_lowercase();
                    let score = if qkey == "dismiss" {
                        ["accept", "close", "dismiss", "ok", "got it"]
                            .iter()
                            .filter(|w| d.contains(**w))
                            .count()
                            * 10
                    } else {
                        target_tokens.iter().filter(|t| d.contains(t.as_str())).count()
                    };
                    if score > best_score {
                        best_score = score;
                        best_key = k.clone();
                    }
                }

                let (choice, conf) = if best_score > 0 {
                    (best_key, 0.93)
                } else if criteria.contains_key("scroll_down")
                    && context_text.contains("can_scroll_down=true")
                {
                    ("scroll_down".to_string(), 0.88)
                } else if criteria.contains_key("none") {
                    ("none".to_string(), 0.91)
                } else {
                    (criteria.keys().next().cloned().unwrap_or_default(), 0.30)
                };

                let mut probs = BTreeMap::new();
                for k in criteria.keys() {
                    probs.insert(k.clone(), if k == &choice { conf } else { 0.01 });
                }

                answers.insert(
                    qkey.clone(),
                    Answer::Choice {
                        choice,
                        probabilities: probs,
                        confidence: conf,
                        applicability: Some(1.0),
                    },
                );
            }
        }
    }

    DecisionResponse {
        answers,
        usage: Some(Usage {
            total_tokens: 512,
            total_input_tokens: 512,
            total_output_tokens: 0,
        }),
    }
}

fn extract_quoted(s: &str) -> Option<String> {
    let start = s.find('"')?;
    let rest = &s[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_keywords(s: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "to", "for", "of", "in", "on", "is", "are", "this", "that", "button",
        "field", "input", "click", "select", "enter", "type", "dropdown", "box", "page",
        "currently", "true", "statement", "following", "evaluate", "whether", "web", "row",
    ];
    s.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.trim().to_lowercase())
        .filter(|w| w.len() >= 2 && !STOP.contains(&w.as_str()))
        .collect()
}

fn expand_synonyms(tokens: &[String]) -> Vec<String> {
    let mut out = tokens.to_vec();
    for t in tokens {
        match t.as_str() {
            "sign" | "signin" | "login" => {
                out.extend(["sign", "log", "account", "submit"].map(String::from));
            }
            "username" | "user" => {
                out.extend(["username", "handle", "account"].map(String::from));
            }
            _ => {}
        }
    }
    out
}
