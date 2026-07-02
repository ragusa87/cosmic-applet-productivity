use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::atomic;
use crate::models::{Provider, ProviderSnapshot, SpendInfo, UsageWindow};

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const REFRESH_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-code/2.1.112";

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CredentialEnvelope {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: ClaudeOAuthCredentials,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClaudeOAuthCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(
        rename = "refreshToken",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    refresh_token: Option<String>,
    /// Milliseconds since epoch, matching Claude Code's on-disk format.
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(
        rename = "subscriptionType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    subscription_type: Option<String>,
    #[serde(
        rename = "rateLimitTier",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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
    let data = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let env: CredentialEnvelope =
        serde_json::from_slice(&data).with_context(|| format!("parse {}", path.display()))?;
    Ok(env.claude_ai_oauth)
}

fn save_credentials(creds: &ClaudeOAuthCredentials) -> Result<()> {
    let path = credentials_path()?;
    let env = CredentialEnvelope {
        claude_ai_oauth: creds.clone(),
    };
    let serialized = serde_json::to_vec_pretty(&env)?;
    atomic::write_preserving_mode(&path, &serialized, 0o600)
        .with_context(|| format!("atomic write {}", path.display()))?;
    Ok(())
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
        refresh_token: refreshed
            .refresh_token
            .or_else(|| creds.refresh_token.clone()),
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
    #[serde(default)]
    spend: Option<SpendPayload>,
}

#[derive(Debug, Deserialize)]
struct UsageWindowPayload {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

/// Post-plan usage-credit spend block. All fields are optional so an account
/// without extra usage (or a future response reshape) never breaks parsing.
#[derive(Debug, Deserialize)]
struct SpendPayload {
    #[serde(default)]
    used: Option<MoneyPayload>,
    #[serde(default)]
    limit: Option<MoneyPayload>,
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    enabled: bool,
}

// Money as minor units + exponent, e.g. `{amount_minor: 3877, exponent: 2}` = $38.77.
#[derive(Debug, Deserialize)]
struct MoneyPayload {
    #[serde(default)]
    amount_minor: Option<i64>,
    #[serde(default)]
    exponent: Option<u32>,
    #[serde(default)]
    currency: Option<String>,
}

// Convert minor units to a major-unit amount (`amount_minor / 10^exponent`).
// Exponent defaults to 2 when absent; returns `None` if no amount is present.
#[allow(clippy::cast_precision_loss)]
fn money_major(m: &MoneyPayload) -> Option<f64> {
    let amount = m.amount_minor?;
    let exponent = i32::try_from(m.exponent.unwrap_or(2)).ok()?;
    Some(amount as f64 / 10f64.powi(exponent))
}

// Map the raw spend payload into the display model. Returns `None` when there
// is no `used` amount to show; otherwise carries `enabled` through so the UI
// decides visibility.
fn map_spend(payload: &SpendPayload) -> Option<SpendInfo> {
    let used_money = payload.used.as_ref()?;
    let used = money_major(used_money)?;
    let limit = payload.limit.as_ref().and_then(money_major);
    let percent = payload.percent.unwrap_or_else(|| match limit {
        Some(limit) if limit > 0.0 => used / limit * 100.0,
        _ => 0.0,
    });
    let currency = used_money
        .currency
        .clone()
        .unwrap_or_else(|| "USD".to_owned());
    Some(SpendInfo {
        used,
        limit,
        percent,
        currency,
        enabled: payload.enabled,
    })
}

fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

async fn fetch_usage(client: &reqwest::Client, access_token: &str) -> Result<UsageResponse> {
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
        if let Err(e) = save_credentials(&creds) {
            tracing::warn!(error = %e, "failed to persist refreshed Anthropic credentials");
        }
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
            if let Err(e) = save_credentials(&creds) {
                tracing::warn!(error = %e, "failed to persist refreshed Anthropic credentials");
            }
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

    let spend = usage.spend.as_ref().and_then(map_spend);

    Ok(ProviderSnapshot {
        provider: Provider::Anthropic,
        short,
        weekly,
        spend,
    })
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn real_response_spend_maps_to_dollars() {
        // Live /api/oauth/usage payload (trimmed to the relevant fields).
        let json = r#"{
            "five_hour": { "utilization": 100.0, "resets_at": "2026-07-02T11:40:00+00:00" },
            "seven_day": { "utilization": 14.0, "resets_at": "2026-07-07T22:00:00+00:00" },
            "spend": {
                "used":  { "amount_minor": 3877, "currency": "USD", "exponent": 2 },
                "limit": { "amount_minor": 8000, "currency": "USD", "exponent": 2 },
                "percent": 48, "enabled": true
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).expect("parse");
        let spend = map_spend(&usage.spend.expect("spend present")).expect("mapped");
        assert!(approx(spend.used, 38.77), "used = {}", spend.used);
        assert_eq!(spend.limit, Some(80.00));
        assert!(approx(spend.percent, 48.0));
        assert_eq!(spend.currency, "USD");
        assert!(spend.enabled);
    }

    #[test]
    fn money_major_decodes_exponent() {
        let m = MoneyPayload {
            amount_minor: Some(3877),
            exponent: Some(2),
            currency: None,
        };
        assert!(approx(money_major(&m).unwrap(), 38.77));

        // exponent 0 => integer value
        let whole = MoneyPayload {
            amount_minor: Some(80),
            exponent: Some(0),
            currency: None,
        };
        assert!(approx(money_major(&whole).unwrap(), 80.0));

        // missing exponent defaults to 2
        let default_exp = MoneyPayload {
            amount_minor: Some(3877),
            exponent: None,
            currency: None,
        };
        assert!(approx(money_major(&default_exp).unwrap(), 38.77));

        // missing amount => None
        let empty = MoneyPayload {
            amount_minor: None,
            exponent: Some(2),
            currency: None,
        };
        assert!(money_major(&empty).is_none());
    }

    #[test]
    fn percent_falls_back_to_ratio() {
        let payload = SpendPayload {
            used: Some(MoneyPayload {
                amount_minor: Some(2000),
                exponent: Some(2),
                currency: Some("USD".to_owned()),
            }),
            limit: Some(MoneyPayload {
                amount_minor: Some(8000),
                exponent: Some(2),
                currency: Some("USD".to_owned()),
            }),
            percent: None,
            enabled: true,
        };
        let spend = map_spend(&payload).expect("mapped");
        assert!(approx(spend.percent, 25.0), "percent = {}", spend.percent);
    }

    #[test]
    fn percent_no_divide_by_zero_without_limit() {
        let payload = SpendPayload {
            used: Some(MoneyPayload {
                amount_minor: Some(2000),
                exponent: Some(2),
                currency: None,
            }),
            limit: None,
            percent: None,
            enabled: true,
        };
        let spend = map_spend(&payload).expect("mapped");
        assert_eq!(spend.limit, None);
        assert!(approx(spend.percent, 0.0));
        assert_eq!(spend.currency, "USD"); // defaulted
    }

    #[test]
    fn spend_without_used_maps_to_none() {
        let payload = SpendPayload {
            used: None,
            limit: Some(MoneyPayload {
                amount_minor: Some(8000),
                exponent: Some(2),
                currency: None,
            }),
            percent: Some(0.0),
            enabled: true,
        };
        assert!(map_spend(&payload).is_none());
    }

    #[test]
    fn disabled_spend_still_maps() {
        let payload = SpendPayload {
            used: Some(MoneyPayload {
                amount_minor: Some(0),
                exponent: Some(2),
                currency: Some("USD".to_owned()),
            }),
            limit: None,
            percent: Some(0.0),
            enabled: false,
        };
        let spend = map_spend(&payload).expect("mapped");
        assert!(!spend.enabled); // UI hides it, but the mapping succeeds
    }

    #[test]
    fn response_without_spend_still_parses() {
        // Regression guard: accounts without extra usage omit `spend` entirely.
        let json = r#"{
            "five_hour": { "utilization": 12.0, "resets_at": "2026-07-02T11:40:00+00:00" },
            "seven_day": { "utilization": 3.0, "resets_at": "2026-07-07T22:00:00+00:00" }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).expect("parse");
        assert!(usage.spend.is_none());
    }
}
