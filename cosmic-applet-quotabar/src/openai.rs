use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::models::{Provider, ProviderSnapshot, UsageWindow};

const REFRESH_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const USAGE_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_AFTER_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone)]
struct OpenAiCredentials {
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
    id_token: Option<String>,
    last_refresh: Option<DateTime<Utc>>,
}

impl OpenAiCredentials {
    fn should_refresh_proactively(&self) -> bool {
        match self.last_refresh {
            None => self.refresh_token.as_deref().is_some_and(|t| !t.is_empty()),
            Some(t) => {
                let age = chrono::Utc::now().signed_duration_since(t).num_seconds();
                age > REFRESH_AFTER_SECS
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct AuthFile {
    #[serde(default)]
    last_refresh: Option<String>,
    #[serde(default, rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    #[serde(default)]
    tokens: Option<AuthFileTokens>,
}

#[derive(Debug, Deserialize)]
struct AuthFileTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

fn auth_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
    Ok(home.join(".codex").join("auth.json"))
}

fn parse_last_refresh(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn load_credentials() -> Result<OpenAiCredentials> {
    let path = auth_path()?;
    let data = std::fs::read(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let decoded: AuthFile = serde_json::from_slice(&data)
        .with_context(|| format!("parse {}", path.display()))?;

    if let Some(key) = decoded.openai_api_key.as_deref()
        && !key.trim().is_empty()
    {
        return Err(anyhow!(
            "OPENAI_API_KEY auth is not supported for ChatGPT usage polling; use codex OAuth login"
        ));
    }

    let tokens = decoded
        .tokens
        .ok_or_else(|| anyhow!("OpenAI credentials missing in {}", path.display()))?;
    let access_token = tokens
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("OpenAI access_token missing"))?
        .to_owned();

    Ok(OpenAiCredentials {
        access_token,
        refresh_token: tokens.refresh_token.map(|s| s.trim().to_owned()),
        account_id: tokens.account_id.map(|s| s.trim().to_owned()),
        id_token: tokens.id_token.map(|s| s.trim().to_owned()),
        last_refresh: decoded.last_refresh.as_deref().and_then(parse_last_refresh),
    })
}

fn save_refreshed(creds: &OpenAiCredentials) -> Result<()> {
    let path = auth_path()?;
    let data = std::fs::read(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut json: serde_json::Value =
        serde_json::from_slice(&data).with_context(|| format!("parse {}", path.display()))?;

    let now_iso = chrono::Utc::now().to_rfc3339();
    json["last_refresh"] = serde_json::Value::String(now_iso);
    if let Some(obj) = json.get_mut("tokens").and_then(|v| v.as_object_mut()) {
        obj.insert(
            "access_token".to_owned(),
            serde_json::Value::String(creds.access_token.clone()),
        );
        if let Some(rt) = creds.refresh_token.as_ref() {
            obj.insert(
                "refresh_token".to_owned(),
                serde_json::Value::String(rt.clone()),
            );
        }
        if let Some(id) = creds.id_token.as_ref() {
            obj.insert("id_token".to_owned(), serde_json::Value::String(id.clone()));
        }
    }

    let serialized = serde_json::to_vec_pretty(&json)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: &'a str,
    scope: &'a str,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

async fn refresh(
    client: &reqwest::Client,
    creds: &OpenAiCredentials,
) -> Result<OpenAiCredentials> {
    let refresh_token = creds
        .refresh_token
        .as_deref()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("OpenAI refresh token missing"))?;

    let body = RefreshRequest {
        client_id: CLIENT_ID,
        grant_type: "refresh_token",
        refresh_token,
        scope: "openid profile email",
    };

    let response = client.post(REFRESH_ENDPOINT).json(&body).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("OpenAI refresh HTTP {status}: {body}"));
    }
    let refreshed: RefreshResponse = response.json().await?;
    Ok(OpenAiCredentials {
        access_token: refreshed.access_token,
        refresh_token: refreshed.refresh_token.or_else(|| creds.refresh_token.clone()),
        account_id: creds.account_id.clone(),
        id_token: refreshed.id_token.or_else(|| creds.id_token.clone()),
        last_refresh: Some(chrono::Utc::now()),
    })
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    rate_limit: Option<RateLimit>,
}

#[derive(Debug, Deserialize)]
struct RateLimit {
    #[serde(default)]
    primary_window: Option<Window>,
    #[serde(default)]
    secondary_window: Option<Window>,
}

#[derive(Debug, Deserialize)]
struct Window {
    used_percent: f64,
    reset_at: i64,
}

#[derive(Debug, thiserror::Error)]
enum OpenAiError {
    #[error("OpenAI usage request was unauthorized")]
    Unauthorized,
}

async fn fetch_usage(
    client: &reqwest::Client,
    creds: &OpenAiCredentials,
) -> Result<UsageResponse> {
    let mut req = client
        .get(USAGE_ENDPOINT)
        .bearer_auth(&creds.access_token)
        .header("Accept", "application/json")
        .header("User-Agent", "QuotaBar");
    if let Some(account) = creds.account_id.as_deref()
        && !account.is_empty()
    {
        req = req.header("ChatGPT-Account-Id", account);
    }
    let response = req.send().await?;
    let status = response.status();
    if status.as_u16() == 401 {
        return Err(OpenAiError::Unauthorized.into());
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("OpenAI usage HTTP {status}: {body}"));
    }
    let parsed: UsageResponse = response.json().await?;
    Ok(parsed)
}

fn window_from(payload: &Window) -> UsageWindow {
    let resets_at = Utc.timestamp_opt(payload.reset_at, 0).single();
    UsageWindow {
        used_percent: payload.used_percent,
        resets_at,
    }
}

pub async fn fetch_snapshot(client: &reqwest::Client) -> Result<ProviderSnapshot> {
    let mut creds = load_credentials()?;

    if creds.should_refresh_proactively() {
        match refresh(client, &creds).await {
            Ok(new) => {
                if let Err(e) = save_refreshed(&new) {
                    tracing::warn!(error = %e, "failed to persist refreshed OpenAI credentials");
                }
                creds = new;
            }
            Err(e) => {
                tracing::warn!(error = %e, "OpenAI proactive refresh failed; falling back to existing token");
            }
        }
    }

    let usage = match fetch_usage(client, &creds).await {
        Ok(u) => u,
        Err(e) => {
            let is_unauthorized = e
                .downcast_ref::<OpenAiError>()
                .is_some_and(|inner| matches!(inner, OpenAiError::Unauthorized));
            if !is_unauthorized {
                return Err(e);
            }
            creds = refresh(client, &creds).await?;
            if let Err(e) = save_refreshed(&creds) {
                tracing::warn!(error = %e, "failed to persist refreshed OpenAI credentials");
            }
            fetch_usage(client, &creds).await?
        }
    };

    let (short, weekly) = match usage.rate_limit {
        Some(rl) => (
            rl.primary_window.as_ref().map(window_from),
            rl.secondary_window.as_ref().map(window_from),
        ),
        None => (None, None),
    };

    Ok(ProviderSnapshot {
        provider: Provider::OpenAi,
        short,
        weekly,
    })
}
