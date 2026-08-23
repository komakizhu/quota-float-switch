use std::{fs, path::PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::models::{ProviderSnapshot, UsageWindow};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CREDITS_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_AUTH_BYTES: u64 = 256 * 1024;

struct Auth {
    access_token: String,
    account_id: Option<String>,
}

fn auth_path() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .map(|home| home.join("auth.json"))
}

fn pick_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| value.get(*key)?.as_str())
}

fn plan_label(value: &Value) -> Option<String> {
    pick_string(
        value,
        &["plan_type", "planType", "plan", "plan_name", "planName"],
    )
    .map(|plan| plan.trim().to_uppercase())
    .filter(|plan| !plan.is_empty())
}

fn plan_from_usage(usage: &Value, rate_limit: &Value) -> Option<String> {
    plan_label(usage)
        .or_else(|| plan_label(rate_limit))
        .or_else(|| usage.get("account").and_then(plan_label))
        .or_else(|| usage.get("subscription").and_then(plan_label))
        .or_else(|| rate_limit.get("account").and_then(plan_label))
        .or_else(|| rate_limit.get("subscription").and_then(plan_label))
}

fn account_identifier_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    pick_string(
        &value,
        &[
            "https://api.openai.com/auth.chatgpt_account_id",
            "chatgpt_account_id",
        ],
    )
    .filter(|value| !value.trim().is_empty())
    .or_else(|| pick_string(&value, &["sub"]))
    .map(str::to_owned)
    .filter(|value| !value.trim().is_empty())
}

fn quota_history_scope(identifier: Option<&str>) -> Option<String> {
    let identifier = identifier?.trim();
    if identifier.is_empty() {
        return None;
    }
    let mut input = b"quota-pro:quota-history-scope:v1\0codex\0".to_vec();
    input.extend_from_slice(identifier.as_bytes());
    let digest = Sha256::digest(input);
    Some(URL_SAFE_NO_PAD.encode(&digest[..16]))
}

fn load_auth() -> Result<Auth, &'static str> {
    let path = auth_path().ok_or("Codex login was not found.")?;
    let metadata = fs::metadata(&path).map_err(|_| "Please sign in to Codex Desktop first.")?;
    if !metadata.is_file() || metadata.len() > MAX_AUTH_BYTES {
        return Err("Codex login data is unavailable.");
    }
    let raw = fs::read_to_string(path).map_err(|_| "Please sign in to Codex Desktop first.")?;
    let value: Value = serde_json::from_str(&raw).map_err(|_| "Codex login format has changed.")?;
    let tokens = value.get("tokens").unwrap_or(&value);
    let access_token = pick_string(tokens, &["access_token", "accessToken"])
        .ok_or("Codex login expired. Please sign in again.")?
        .to_owned();
    let account_id = pick_string(tokens, &["account_id", "accountId"])
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| account_identifier_from_jwt(&access_token));
    Ok(Auth {
        access_token,
        account_id,
    })
}

fn headers(auth: &Auth) -> Result<HeaderMap, &'static str> {
    let mut result = HeaderMap::new();
    let mut bearer = HeaderValue::from_str(&format!("Bearer {}", auth.access_token))
        .map_err(|_| "Codex login data is invalid.")?;
    bearer.set_sensitive(true);
    result.insert(AUTHORIZATION, bearer);
    result.insert(ACCEPT, HeaderValue::from_static("application/json"));
    result.insert("originator", HeaderValue::from_static("Codex Desktop"));
    result.insert("OAI-Product-Sku", HeaderValue::from_static("CODEX"));
    if let Some(account_id) = &auth.account_id {
        let mut value =
            HeaderValue::from_str(account_id).map_err(|_| "Account identifier is invalid.")?;
        value.set_sensitive(true);
        result.insert("ChatGPT-Account-Id", value);
    }
    Ok(result)
}

fn number_with_key<'a>(value: &'a Value, keys: &[&'a str]) -> Option<(&'a str, f64)> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_f64().map(|number| (*key, number)))
}

fn integer(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|item| u64::try_from(item).ok()))
    })
}

fn timestamp(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let item = value.get(*key)?;
        if let Some(text) = item.as_str() {
            return Some(text.to_owned());
        }
        item.as_i64()
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
            .map(|time| time.to_rfc3339())
    })
}

fn collect_reset_credit_expirations(value: &Value) -> Vec<String> {
    fn visit(value: &Value, output: &mut Vec<String>) {
        match value {
            Value::Array(items) => {
                for item in items {
                    visit(item, output);
                }
            }
            Value::Object(map) => {
                if let Some(time) = timestamp(
                    value,
                    &[
                        "expires_at",
                        "expiresAt",
                        "expiration_time",
                        "expirationTime",
                        "expires",
                    ],
                ) {
                    output.push(time);
                }
                for key in [
                    "credits",
                    "reset_credits",
                    "resetCredits",
                    "available",
                    "items",
                    "grants",
                ] {
                    if let Some(child) = map.get(key) {
                        visit(child, output);
                    }
                }
            }
            _ => {}
        }
    }

    let mut expirations = Vec::new();
    visit(value, &mut expirations);
    expirations.sort();
    expirations.dedup();
    expirations
}

fn scale_ratio_field(key: &str, value: f64) -> bool {
    matches!(
        key,
        "remaining_ratio" | "remainingRatio" | "used_ratio" | "usedRatio" | "utilization"
    ) || (!key.contains("percent") && !key.contains("pct") && value <= 1.0)
}

fn parse_window(value: Option<&Value>) -> Option<UsageWindow> {
    let value = value?;
    let remaining_percent = if let Some((key, remaining)) = number_with_key(
        value,
        &[
            "remaining_percent",
            "remainingPercent",
            "remaining_pct",
            "remainingPct",
            "remaining_ratio",
            "remainingRatio",
            "remaining",
        ],
    ) {
        if scale_ratio_field(key, remaining) {
            remaining * 100.0
        } else {
            remaining
        }
    } else {
        let (key, used) = number_with_key(
            value,
            &[
                "used_percent",
                "usedPercent",
                "used_pct",
                "usedPct",
                "used_ratio",
                "usedRatio",
                "utilization",
                "used",
            ],
        )?;
        let used_percent = if scale_ratio_field(key, used) {
            used * 100.0
        } else {
            used
        };
        100.0 - used_percent
    };
    Some(UsageWindow {
        remaining_percent: remaining_percent.clamp(0.0, 100.0),
        resets_at: timestamp(
            value,
            &[
                "reset_at",
                "resetAt",
                "resets_at",
                "resetsAt",
                "reset_time",
                "resetTime",
            ],
        ),
        window_seconds: integer(
            value,
            &[
                "limit_window_seconds",
                "limitWindowSeconds",
                "window_seconds",
                "windowSeconds",
                "duration_seconds",
                "durationSeconds",
                "period_seconds",
                "periodSeconds",
            ],
        )
        .unwrap_or(0),
    })
}

fn find_window<'a>(
    rate_limit: &'a Value,
    names: &[&str],
    expected_seconds: u64,
) -> Option<&'a Value> {
    for name in names {
        if let Some(value) = rate_limit.get(*name) {
            let Some(window) = parse_window(Some(value)) else {
                continue;
            };
            if window.window_seconds == 0
                || (expected_seconds > 0 && window.window_seconds.abs_diff(expected_seconds) <= 60)
            {
                return Some(value);
            }
        }
    }

    for key in [
        "windows",
        "limit_windows",
        "limitWindows",
        "limits",
        "buckets",
    ] {
        let Some(items) = rate_limit.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(window) = parse_window(Some(item)) else {
                continue;
            };
            let matches_duration =
                expected_seconds > 0 && window.window_seconds.abs_diff(expected_seconds) <= 60;
            let matches_name = pick_string(item, &["name", "type", "id", "window", "label"])
                .map(|text| {
                    let lower = text.to_ascii_lowercase();
                    names.iter().any(|name| {
                        lower == name.to_ascii_lowercase()
                            || lower.contains(&name.to_ascii_lowercase())
                    })
                })
                .unwrap_or(false);
            // A named array item is only a safe fallback when its duration is
            // absent. If it reports a known but different duration, do not
            // let the label make a five-hour bucket look like a weekly one.
            if matches_duration || (window.window_seconds == 0 && matches_name) {
                return Some(item);
            }
        }
    }

    None
}

fn safe_http_failure(status: reqwest::StatusCode) -> (&'static str, &'static str) {
    match status.as_u16() {
        401 | 403 => ("signed_out", "Codex login expired. Please sign in again."),
        429 => (
            "unavailable",
            "Quota service is rate limited. It will retry automatically.",
        ),
        _ => ("unavailable", "Quota service is temporarily unavailable."),
    }
}

fn snapshot_from_http_failure(
    status_code: reqwest::StatusCode,
    quota_history_scope: Option<String>,
) -> ProviderSnapshot {
    let (status, message) = safe_http_failure(status_code);
    if status == "signed_out" {
        ProviderSnapshot::failure(status, message)
    } else {
        ProviderSnapshot::failure_with_scope(status, message, quota_history_scope)
    }
}

async fn limited_json(mut response: reqwest::Response) -> Result<Value, ()> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len().saturating_add(chunk.len()) as u64 > MAX_RESPONSE_BYTES {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| ())
}

pub async fn fetch_snapshot(client: &reqwest::Client) -> ProviderSnapshot {
    let auth = match load_auth() {
        Ok(value) => value,
        Err(message) => return ProviderSnapshot::failure("signed_out", message),
    };
    let quota_history_scope = quota_history_scope(auth.account_id.as_deref());
    let request_headers = match headers(&auth) {
        Ok(value) => value,
        Err(message) => return ProviderSnapshot::failure("signed_out", message),
    };

    let (usage_result, credits_result) = tokio::join!(
        client
            .get(USAGE_URL)
            .headers(request_headers.clone())
            .send(),
        client.get(CREDITS_URL).headers(request_headers).send(),
    );

    let usage_response = match usage_result {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            return snapshot_from_http_failure(response.status(), quota_history_scope.clone());
        }
        Err(_) => {
            return ProviderSnapshot::failure_with_scope(
                "unavailable",
                "Network unavailable. It will retry automatically.",
                quota_history_scope.clone(),
            )
        }
    };
    let usage: Value = match limited_json(usage_response).await {
        Ok(value) => value,
        Err(_) => {
            return ProviderSnapshot::failure_with_scope(
                "unavailable",
                "Quota response format has changed.",
                quota_history_scope.clone(),
            )
        }
    };
    let rate_limit = usage
        .get("rate_limit")
        .or_else(|| usage.get("rateLimit"))
        .unwrap_or(&usage);
    let short_window = parse_window(find_window(
        rate_limit,
        &[
            "primary_window",
            "primaryWindow",
            "short_window",
            "shortWindow",
            "five_hour_window",
            "fiveHourWindow",
            "5h",
            "primary",
        ],
        18_000,
    ));
    let weekly_window = parse_window(find_window(
        rate_limit,
        &[
            "secondary_window",
            "secondaryWindow",
            "weekly_window",
            "weeklyWindow",
            "week_window",
            "weekWindow",
            "weekly",
            "secondary",
            "primary_window",
            "primaryWindow",
            "primary",
        ],
        604_800,
    ));
    if short_window.is_none() && weekly_window.is_none() {
        return ProviderSnapshot::failure_with_scope(
            "unavailable",
            "Quota response does not contain a recognized usage window.",
            quota_history_scope.clone(),
        );
    }

    let usage_credits = usage
        .get("rate_limit_reset_credits")
        .or_else(|| usage.get("rateLimitResetCredits"));
    let usage_reset_credits = usage_credits.and_then(|value| {
        integer(
            value,
            &[
                "available_count",
                "availableCount",
                "remaining",
                "count",
                "quantity",
            ],
        )
    });
    let usage_reset_credit_expires_at = usage_credits
        .map(collect_reset_credit_expirations)
        .unwrap_or_default();

    let (reset_credits, reset_credit_expires_at) = match credits_result {
        Ok(response) if response.status().is_success() => match limited_json(response).await.ok() {
            Some(value) => (
                integer(
                    &value,
                    &[
                        "available_count",
                        "availableCount",
                        "remaining",
                        "count",
                        "quantity",
                    ],
                )
                .or(usage_reset_credits),
                {
                    let expirations = collect_reset_credit_expirations(&value);
                    if expirations.is_empty() {
                        usage_reset_credit_expires_at
                    } else {
                        expirations
                    }
                },
            ),
            None => (usage_reset_credits, usage_reset_credit_expires_at),
        },
        _ => (usage_reset_credits, usage_reset_credit_expires_at),
    };

    ProviderSnapshot {
        provider: "codex".into(),
        display_name: "CODEX".into(),
        plan: plan_from_usage(&usage, rate_limit),
        quota_history_scope,
        short_window,
        weekly_window,
        reset_credits,
        reset_credit_expires_at,
        updated_at: chrono::Utc::now().to_rfc3339(),
        status: "ok".into(),
        message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_window_shapes() {
        let snake = serde_json::json!({
            "used_percent": 26,
            "reset_at": 1738300000,
            "limit_window_seconds": 18000
        });
        let window = parse_window(Some(&snake)).unwrap();
        assert_eq!(window.remaining_percent, 74.0);
        assert_eq!(window.window_seconds, 18000);
        let camel = serde_json::json!({
            "utilization": 0.4,
            "resetsAt": "2026-07-07T00:00:00Z",
            "windowSeconds": 604800
        });
        assert_eq!(parse_window(Some(&camel)).unwrap().remaining_percent, 60.0);
    }

    #[test]
    fn prefers_explicit_remaining_percent() {
        let value = serde_json::json!({
            "remainingPercent": 73.4,
            "usedPercent": 99,
            "resetTime": "2026-07-07T00:00:00Z",
            "durationSeconds": 18000
        });
        let window = parse_window(Some(&value)).unwrap();
        assert_eq!(window.remaining_percent, 73.4);
        assert_eq!(window.window_seconds, 18000);
    }

    #[test]
    fn treats_fractional_percent_fields_as_ratios() {
        let explicit_remaining = serde_json::json!({"remaining": 0.25, "periodSeconds": 18000});
        assert_eq!(
            parse_window(Some(&explicit_remaining))
                .unwrap()
                .remaining_percent,
            25.0
        );

        let used_ratio = serde_json::json!({"used": 0.25, "periodSeconds": 18000});
        assert_eq!(
            parse_window(Some(&used_ratio)).unwrap().remaining_percent,
            75.0
        );
    }

    #[test]
    fn does_not_scale_explicit_percent_fields() {
        let explicit_remaining =
            serde_json::json!({"remaining_percent": 0.4, "windowSeconds": 18000});
        assert_eq!(
            parse_window(Some(&explicit_remaining))
                .unwrap()
                .remaining_percent,
            0.4
        );

        let explicit_used = serde_json::json!({"used_percent": 0.4, "windowSeconds": 18000});
        assert_eq!(
            parse_window(Some(&explicit_used))
                .unwrap()
                .remaining_percent,
            99.6
        );
    }

    #[test]
    fn finds_window_by_duration_or_name_in_arrays() {
        let rate_limit = serde_json::json!({
            "windows": [
                {"name": "weekly", "remainingPercent": 88, "windowSeconds": 604800},
                {"name": "primary", "remainingPercent": 51, "windowSeconds": 18000}
            ]
        });
        let short = parse_window(find_window(
            &rate_limit,
            &["primary_window", "primary"],
            18_000,
        ))
        .unwrap();
        let weekly = parse_window(find_window(
            &rate_limit,
            &["secondary_window", "weekly"],
            604_800,
        ))
        .unwrap();
        assert_eq!(short.remaining_percent, 51.0);
        assert_eq!(weekly.remaining_percent, 88.0);
    }

    #[test]
    fn does_not_treat_a_weekly_primary_field_as_a_short_window() {
        let value = serde_json::json!({
            "primary_window": {"remainingPercent": 98, "windowSeconds": 604800},
            "weekly_window": {"remainingPercent": 98, "windowSeconds": 604800}
        });
        assert!(find_window(&value, &["primary_window", "primary"], 18_000).is_none());
        assert!(find_window(&value, &["weekly_window", "weekly"], 604_800).is_some());
    }

    #[test]
    fn recognizes_a_weekly_primary_field_as_weekly_fallback() {
        let value = serde_json::json!({
            "primary": {"remainingPercent": 98, "windowSeconds": 604800}
        });
        let weekly = parse_window(find_window(
            &value,
            &["weekly_window", "weekly", "primary_window", "primary"],
            604_800,
        ))
        .unwrap();
        assert_eq!(weekly.remaining_percent, 98.0);
        assert_eq!(weekly.window_seconds, 604_800);
    }

    #[test]
    fn does_not_match_a_named_weekly_item_with_a_short_duration() {
        let value = serde_json::json!({
            "windows": [
                {"name": "weekly", "remainingPercent": 88, "windowSeconds": 18000}
            ]
        });
        assert!(find_window(&value, &["weekly_window", "weekly"], 604800).is_none());
    }

    #[test]
    fn reads_plan_labels_from_supported_usage_shapes() {
        let top_level = serde_json::json!({"plan_type": "pro_5x"});
        assert_eq!(
            plan_from_usage(&top_level, &top_level).as_deref(),
            Some("PRO_5X")
        );

        let nested = serde_json::json!({
            "rate_limit": {"planName": "pro 20x"}
        });
        let rate_limit = nested.get("rate_limit").unwrap();
        assert_eq!(
            plan_from_usage(&nested, rate_limit).as_deref(),
            Some("PRO 20X")
        );

        let account = serde_json::json!({"account": {"plan": "pro lite"}});
        assert_eq!(
            plan_from_usage(&account, &account).as_deref(),
            Some("PRO LITE")
        );

        let nested_account =
            serde_json::json!({"account": {}, "rate_limit": {"subscription": {"plan": "pro 5x"}}});
        let rate_limit = nested_account.get("rate_limit").unwrap();
        assert_eq!(
            plan_from_usage(&nested_account, rate_limit).as_deref(),
            Some("PRO 5X")
        );
    }

    #[test]
    fn derives_the_account_identifier_from_jwt_claim_or_sub() {
        let claim_payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "https://api.openai.com/auth.chatgpt_account_id": "account-claim",
                "sub": "subject-account"
            }))
            .unwrap(),
        );
        assert_eq!(
            account_identifier_from_jwt(&format!("header.{claim_payload}.signature")),
            Some("account-claim".into())
        );

        let sub_payload = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&serde_json::json!({"sub": "subject-account"})).unwrap());
        assert_eq!(
            account_identifier_from_jwt(&format!("header.{sub_payload}.signature")),
            Some("subject-account".into())
        );

        let blank_claim_payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "https://api.openai.com/auth.chatgpt_account_id": "  ",
                "sub": "subject-fallback"
            }))
            .unwrap(),
        );
        assert_eq!(
            account_identifier_from_jwt(&format!("header.{blank_claim_payload}.signature")),
            Some("subject-fallback".into())
        );
    }

    #[test]
    fn quota_history_scope_is_stable_and_does_not_expose_the_identifier() {
        let first = quota_history_scope(Some("account-a")).unwrap();
        let same = quota_history_scope(Some("account-a")).unwrap();
        let other = quota_history_scope(Some("account-b")).unwrap();

        assert_eq!(first, same);
        assert_ne!(first, other);
        assert_eq!(first.len(), 22);
        assert!(first.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        }));
        assert!(!first.contains("account-a"));
        assert!(quota_history_scope(None).is_none());
        assert!(quota_history_scope(Some("  ")).is_none());
    }

    #[test]
    fn snapshot_serialization_exposes_only_the_hashed_scope() {
        let snapshot = ProviderSnapshot::failure_with_scope(
            "unavailable",
            "Network unavailable.",
            Some("scope-value".into()),
        );
        let json = serde_json::to_string(&snapshot).unwrap();

        assert!(json.contains("\"quotaHistoryScope\":\"scope-value\""));
        assert!(!json.contains("account_id"));
        assert!(!json.contains("access_token"));
        assert!(!json.contains("Bearer"));
    }

    #[test]
    fn signed_out_http_failures_do_not_retain_a_scope() {
        let signed_out = snapshot_from_http_failure(
            reqwest::StatusCode::UNAUTHORIZED,
            Some("scope-value".into()),
        );
        assert_eq!(signed_out.status, "signed_out");
        assert!(signed_out.quota_history_scope.is_none());

        let unavailable = snapshot_from_http_failure(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("scope-value".into()),
        );
        assert_eq!(unavailable.status, "unavailable");
        assert_eq!(
            unavailable.quota_history_scope.as_deref(),
            Some("scope-value")
        );
    }
}
