//! A small, explicit check that the configured OpenAI-compatible endpoint can
//! answer a real completion before the user saves the settings.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::tools::cap;

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_DETAIL_CHARS: usize = 2_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTest {
    model: String,
    latency_ms: u64,
}

#[tauri::command]
pub async fn test_connection(
    base_url: String,
    api_key: String,
    model: String,
) -> Result<ConnectionTest, String> {
    check(&base_url, &api_key, &model).await
}

async fn check(base_url: &str, api_key: &str, model: &str) -> Result<ConnectionTest, String> {
    let base_url = base_url.trim().trim_end_matches('/');
    let api_key = api_key.trim();
    let model = model.trim();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err("Base URL must start with http:// or https://.".into());
    }
    if model.is_empty() {
        return Err("Model is required.".into());
    }

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|err| format!("Could not prepare the connection test: {err}"))?;
    let url = format!("{base_url}/chat/completions");
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 1,
        "stream": false,
    });
    let mut request = client.post(&url).json(&body);
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }

    let started = Instant::now();
    let response = request
        .send()
        .await
        .map_err(|err| format!("Could not reach {base_url}: {err}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(http_error(status, &text));
    }

    let value: Value = serde_json::from_str(&text)
        .map_err(|_| "The provider returned a response that was not JSON.".to_string())?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Err(format!(
            "Provider error: {}",
            cap(error.to_string(), MAX_DETAIL_CHARS)
        ));
    }

    // Gateways sometimes omit `model` on a successful response; the requested
    // name still tells the user what was tested.
    let answered = value
        .get("model")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(model);
    Ok(ConnectionTest {
        model: answered.to_string(),
        latency_ms: started.elapsed().as_millis() as u64,
    })
}

fn http_error(status: reqwest::StatusCode, body: &str) -> String {
    let hint = match status.as_u16() {
        401 => "Authentication failed. Check the API key.",
        403 => "The provider refused the request. Check the API key and permissions.",
        404 => "Endpoint not found. Check the Base URL.",
        429 => "The provider is rate limiting requests; try again shortly.",
        _ => "",
    };
    let mut message = format!("Model request failed ({status}).");
    if !hint.is_empty() {
        message.push(' ');
        message.push_str(hint);
    }
    let detail = body.trim();
    if !detail.is_empty() {
        message.push_str("\n\n");
        message.push_str(&cap(detail.to_string(), MAX_DETAIL_CHARS));
    }
    message
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use serde_json::Value;

    use super::check;

    /// Minimal OpenAI-compatible endpoint that records the raw request and
    /// answers with one canned response.
    fn server(
        status: u16,
        response_body: &str,
    ) -> (String, Arc<Mutex<String>>, thread::JoinHandle<()>) {
        let response_body = response_body.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let seen_by_server = seen.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..read]);
                if let Some(head) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&raw[..head]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if raw.len() >= head + 4 + length {
                        *seen_by_server.lock().unwrap() =
                            String::from_utf8_lossy(&raw).into_owned();
                        break;
                    }
                }
            }
            let reason = if (200..300).contains(&status) {
                "OK"
            } else {
                "Error"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}"), seen, server)
    }

    #[tokio::test]
    async fn rejects_a_missing_url_or_model_without_network_access() {
        let invalid_url = check("ftp://example.com", "key", "model")
            .await
            .unwrap_err();
        assert!(invalid_url.contains("http://"));
        let missing_model = check("https://example.com", "key", " ").await.unwrap_err();
        assert!(missing_model.contains("Model is required"));
    }

    #[tokio::test]
    async fn sends_a_tiny_completion_and_reports_the_answering_model() {
        let (base_url, seen, handle) =
            server(200, r#"{"model":"remote-flash","choices":[],"error":null}"#);
        let result = check(&base_url, "secret", "deepseek-flash").await.unwrap();

        assert_eq!(result.model, "remote-flash");
        let request = seen.lock().unwrap().clone();
        assert!(request.starts_with("POST /chat/completions"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer secret")
        );
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["model"], "deepseek-flash");
        assert_eq!(body["max_tokens"], 1);
        handle.join().unwrap();
    }

    #[tokio::test]
    async fn reports_http_and_in_band_provider_errors() {
        let (base_url, _seen, handle) = server(401, r#"{"error":{"message":"Invalid API key"}}"#);
        let error = check(&base_url, "bad", "model").await.unwrap_err();
        assert!(error.contains("401"));
        assert!(error.contains("Check the API key"));
        assert!(error.contains("Invalid API key"));
        handle.join().unwrap();

        let (base_url, _seen, handle) = server(200, r#"{"error":{"message":"Model not found"}}"#);
        let error = check(&base_url, "bad", "model").await.unwrap_err();
        assert!(error.contains("Model not found"));
        handle.join().unwrap();
    }
}
