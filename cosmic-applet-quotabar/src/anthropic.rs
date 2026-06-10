use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::models::{Provider, ProviderSnapshot, UsageWindow};

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const REFRESH_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-code/2.1.112";

#[derive(Debug, Clone, Deserialize)]
struct CredentialEnvelope {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: ClaudeOAuthCredentials,
}

#[derive(Debug, Clone, Deserialize)]
struct ClaudeOAuthCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken", default)]
    refresh_token: Option<String>,
    /// Milliseconds since epoch, matching Claude Code's on-disk format.
    #[serde(rename = "expiresAt", default)]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(rename = "subscriptionType", default)]
    subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier", default)]
    rate_limit_tier: Option<String>,
}

impl ClaudeOAuthCredentials {
    fn is_expired(&self) -> bool {
        let Some(ms) = self.expires_at else {
            return false;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        now_ms >= ms
    }
}

fn credentials_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
    Ok(home.join(".claude").join(".credentials.json"))
}

fn load_credentials() -> Result<ClaudeOAuthCredentials> {
    let path = credentials_path()?;
    let data = std::fs::read(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let env: CredentialEnvelope = serde_json::from_slice(&data)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(env.claude_ai_oauth)
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    rate_limit_tier: Option<String>,
}

async fn refresh(
    client: &reqwest::Client,
    creds: &ClaudeOAuthCredentials,
) -> Result<ClaudeOAuthCredentials> {
    let refresh_token = creds
        .refresh_token
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Anthropic refresh token missing"))?;

    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ];

    let response = client
        .post(REFRESH_ENDPOINT)
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("Anthropic refresh HTTP {status}: {body}"));
    }

    let refreshed: RefreshResponse = response.json().await?;
    let expires_at = refreshed
        .expires_in
        .map(|secs| chrono::Utc::now().timestamp_millis() + secs * 1000);
    let scopes = refreshed.scope.as_deref().map_or_else(
        || creds.scopes.clone(),
        |s| s.split(' ').map(str::to_owned).collect::<Vec<_>>(),
    );

    Ok(ClaudeOAuthCredentials {
        access_token: refreshed.access_token,
        refresh_token: refreshed.refresh_token.or_else(|| creds.refresh_token.clone()),
        expires_at,
        scopes,
        subscription_type: creds.subscription_type.clone(),
        rate_limit_tier: refreshed
            .rate_limit_tier
            .or_else(|| creds.rate_limit_tier.clone()),
    })
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    five_hour: Option<UsageWindowPayload>,
    #[serde(default)]
    seven_day: Option<UsageWindowPayload>,
}

#[derive(Debug, Deserialize)]
struct UsageWindowPayload {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

async fn fetch_usage(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<UsageResponse> {
    let response = client
        .get(USAGE_ENDPOINT)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", ANTHROPIC_BETA)
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;

    let status = response.status();
    if status.as_u16() == 401 {
        return Err(AnthropicError::Unauthorized.into());
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("Anthropic usage HTTP {status}: {body}"));
    }
    let parsed: UsageResponse = response.json().await?;
    Ok(parsed)
}

#[derive(Debug, thiserror::Error)]
enum AnthropicError {
    #[error("Anthropic usage request was unauthorized")]
    Unauthorized,
}

pub async fn fetch_snapshot(client: &reqwest::Client) -> Result<ProviderSnapshot> {
    let mut creds = load_credentials()?;

    if creds.is_expired() {
        creds = refresh(client, &creds).await?;
    }

    let usage = match fetch_usage(client, &creds.access_token).await {
        Ok(u) => u,
        Err(e) => {
            let is_unauthorized = e
                .downcast_ref::<AnthropicError>()
                .is_some_and(|inner| matches!(inner, AnthropicError::Unauthorized));
            if !is_unauthorized {
                return Err(e);
            }
            creds = refresh(client, &creds).await?;
            fetch_usage(client, &creds.access_token).await?
        }
    };

    let short = usage.five_hour.and_then(|w| {
        let util = w.utilization?;
        Some(UsageWindow {
            used_percent: util,
            resets_at: w.resets_at.as_deref().and_then(parse_iso),
        })
    });
    let weekly = usage.seven_day.and_then(|w| {
        let util = w.utilization?;
        Some(UsageWindow {
            used_percent: util,
            resets_at: w.resets_at.as_deref().and_then(parse_iso),
        })
    });

    Ok(ProviderSnapshot {
        provider: Provider::Anthropic,
        short,
        weekly,
    })
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build reqwest client")
}
