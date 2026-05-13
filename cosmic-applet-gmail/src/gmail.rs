use anyhow::{Context, Result, bail};
use serde::Deserialize;

const LABEL_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/labels/INBOX";

#[derive(Debug, Deserialize)]
struct LabelResponse {
    #[serde(rename = "messagesUnread", default)]
    messages_unread: u64,
}

pub async fn unread_count(access_token: &str) -> Result<u64> {
    let client = reqwest::Client::new();
    let response = client
        .get(LABEL_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .context("call Gmail labels.get")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("gmail labels.get returned {status}: {body}");
    }

    let label: LabelResponse = response.json().await.context("parse Gmail label JSON")?;
    Ok(label.messages_unread)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_messages_unread() {
        let json = r#"{
            "id": "INBOX",
            "name": "INBOX",
            "messageListVisibility": "show",
            "labelListVisibility": "labelShow",
            "type": "system",
            "messagesTotal": 100,
            "messagesUnread": 7,
            "threadsTotal": 80,
            "threadsUnread": 5
        }"#;
        let parsed: LabelResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.messages_unread, 7);
    }

    #[test]
    fn handles_zero_unread() {
        let json = r#"{ "id": "INBOX" }"#;
        let parsed: LabelResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.messages_unread, 0);
    }
}
