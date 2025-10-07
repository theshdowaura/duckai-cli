use anyhow::{anyhow, Context};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::error::Result;
use crate::js;
use crate::model::{EvaluatedHashes, StatusResponse};
use crate::session::HttpSession;
use crate::util::sha256_base64;

/// Represents session preparation output including hashes and FE metadata.
#[derive(Debug, Clone)]
pub struct VqdSession {
    pub vqd_header: String,
    pub fe_version: String,
    pub hashed_client: Vec<String>,
    pub raw_client: Vec<String>,
    pub eval: EvaluatedHashes,
    pub status_body: StatusResponse,
}

#[derive(Debug)]
struct StatusData {
    script_b64: String,
    body: StatusResponse,
}

/// Full VQD preparation sequence: status fetch, script evaluation, and FE metadata parsing.
pub async fn prepare_session(session: &HttpSession) -> Result<VqdSession> {
    let status = fetch_status(session).await?;
    let eval = evaluate_script(&status.script_b64, session.user_agent()).await?;
    let hashed_client = eval
        .client_hashes
        .iter()
        .map(|value| sha256_base64(value))
        .collect::<Vec<_>>();
    let vqd_header = encode_vqd_header(&eval, &hashed_client)?;
    let fe_version = fetch_fe_version(session).await?;

    Ok(VqdSession {
        vqd_header,
        fe_version,
        hashed_client,
        raw_client: eval.client_hashes.clone(),
        eval,
        status_body: status.body,
    })
}

async fn fetch_status(session: &HttpSession) -> Result<StatusData> {
    let url = session
        .base_url()
        .join("duckchat/v1/status")
        .context("invalid status url")?;
    let response = session
        .client()
        .get(url)
        .header("Accept", "application/json")
        .header("x-vqd-accept", "1")
        .send()
        .await
        .context("requesting /duckchat/v1/status")?;

    if !response.status().is_success() {
        return Err(anyhow!("status request failed: {}", response.status()));
    }

    let headers = response.headers();
    let script_b64 = headers
        .get("x-vqd-hash-1")
        .ok_or_else(|| anyhow!("status response missing x-vqd-hash-1 header"))?
        .to_str()
        .context("parsing x-vqd-hash-1 header")?
        .to_owned();

    let body: StatusResponse = response.json().await.context("parsing status body")?;

    Ok(StatusData { script_b64, body })
}

async fn evaluate_script(script_b64: &str, ua: &str) -> Result<EvaluatedHashes> {
    js::evaluate(script_b64, ua).context("executing VQD script via embedded JS runtime")
}

fn encode_vqd_header(eval: &EvaluatedHashes, hashed_client: &[String]) -> Result<String> {
    let payload = serde_json::json!({
        "server_hashes": eval.server_hashes,
        "client_hashes": hashed_client,
        "signals": eval.signals,
        "meta": eval.meta,
    });
    let encoded = BASE64_STANDARD.encode(payload.to_string());
    Ok(encoded)
}

async fn fetch_fe_version(session: &HttpSession) -> Result<String> {
    let url = session
        .base_url()
        .join("?q=DuckDuckGo+AI+Chat&ia=chat&duckai=1")
        .context("invalid fe-version url")?;

    let html = session
        .client()
        .get(url)
        .send()
        .await
        .context("requesting DuckDuckGo homepage")?
        .text()
        .await
        .context("reading homepage body")?;

    extract_fe_version(&html)
}

static BE_VERSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"__DDG_BE_VERSION__\s*=\s*"([^"]+)""#).expect("regex compile"));
static FE_HASH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"__DDG_FE_CHAT_HASH__\s*=\s*"([^"]+)""#).expect("regex compile"));
static FE_SCRIPT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"wpm\.chat\.([^."]+)\.js"#).expect("regex compile"));

fn extract_fe_version(html: &str) -> Result<String> {
    let be = BE_VERSION_RE
        .captures(html)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_owned())
        .ok_or_else(|| anyhow!("missing __DDG_BE_VERSION__ marker"))?;

    let fe_hash = FE_HASH_RE
        .captures(html)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_owned())
        .or_else(|| {
            FE_SCRIPT_RE
                .captures(html)
                .and_then(|caps| caps.get(1))
                .map(|m| m.as_str().to_owned())
        })
        .ok_or_else(|| anyhow!("missing FE hash marker"))?;

    Ok(format!("{be}-{fe_hash}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    #[test]
    fn extracts_fe_version_from_hash() {
        let html = r#"
            <script>__DDG_BE_VERSION__ = "abcdef";</script>
            <script>__DDG_FE_CHAT_HASH__ = "12345";</script>
        "#;
        let version = extract_fe_version(html).unwrap();
        assert_eq!(version, "abcdef-12345");
    }

    #[test]
    fn extracts_fe_version_from_script_fallback() {
        let html = r#"
            <script>__DDG_BE_VERSION__ = "abcdef";</script>
            <script src="/wpm.chat.xyz789.js"></script>
        "#;
        let version = extract_fe_version(html).unwrap();
        assert_eq!(version, "abcdef-xyz789");
    }

    #[test]
    fn fails_when_markers_missing() {
        let err = extract_fe_version("no markers").unwrap_err();
        assert!(err.to_string().contains("missing __DDG_BE_VERSION__"));
    }

    #[tokio::test]
    async fn evaluates_known_script() {
        let script_b64 = include_str!("../../script.b64").trim();
        let result = evaluate_script(script_b64, "FakeUA/1.0")
            .await
            .expect("script should evaluate successfully");
        assert_eq!(result.client_hashes[0], "FakeUA/1.0");
        assert_eq!(result.client_hashes[1], "6419");
        assert_eq!(result.client_hashes[2], "5072");
        assert_eq!(result.server_hashes.len(), 3);
    }

    #[tokio::test]
    async fn errors_for_invalid_script() {
        let bogus = BASE64_STANDARD.encode(b"hello");
        let err = evaluate_script(&bogus, "UA").await.unwrap_err();
        assert!(err.to_string().contains("JS evaluation failed"));
    }
}
