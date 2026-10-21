//
// Copyright (c) 2025-2026 rustmailer.com (https://rustmailer.com)
//
// This file is part of the Bichon Email Archiving Project
//
// These tests exercise the Prometheus / OpenMetrics endpoint: access control
// (no token / no permission / `system:metrics` / admin) and the shape of the
// exposition. The scrape-only case walks the documented setup end to end:
// role with only `system:metrics` → user → API token created by an admin.

use std::collections::{BTreeMap, BTreeSet};

use bichon_core::users::role::DEFAULT_MEMBER_ROLE_ID;
use poem::{http::StatusCode, test::TestClient};
use serde::{Deserialize, Serialize};

use super::{admin_token, build_api_route, setup, LoginPayload, LoginResult};

fn api_client(route: impl poem::Endpoint) -> TestClient<impl poem::Endpoint> {
    TestClient::new(route).default_header("X-Forwarded-For", "127.0.0.1")
}

#[derive(Debug, Serialize)]
struct CreateRolePayload {
    name: String,
    role_type: String,
    permissions: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
struct CreateUserPayload {
    username: String,
    email: String,
    password: String,
    global_roles: Vec<u64>,
    account_access_map: BTreeMap<u64, u64>,
}

#[derive(Debug, Serialize)]
struct CreateTokenPayload {
    name: String,
    user_id: u64,
}

#[derive(Debug, Deserialize)]
struct IdView {
    id: u64,
}

const PASSWORD: &str = "testpass123";

async fn login(username: &str) -> String {
    let route = poem::Route::new().at("/api/login", poem::post(crate::rest::public::login::login));
    let resp = TestClient::new(route)
        .post("/api/login")
        .body_json(&LoginPayload {
            username: username.into(),
            password: PASSWORD.into(),
        })
        .send()
        .await;
    resp.assert_status_is_ok();
    let result: LoginResult = resp.json().await.value().deserialize();
    assert!(result.success, "login failed for {username}");
    result.access_token.expect("access_token should be present")
}

async fn create_metrics_role(token: &str) -> u64 {
    let resp = api_client(build_api_route())
        .post("/api/v1/roles")
        .header("Authorization", format!("Bearer {token}"))
        .body_json(&CreateRolePayload {
            name: "metrics-scraper".into(),
            role_type: "Global".into(),
            permissions: BTreeSet::from(["system:metrics".to_string()]),
        })
        .send()
        .await;
    resp.assert_status_is_ok();
    resp.json().await.value().deserialize::<IdView>().id
}

async fn create_user(token: &str, username: &str, global_roles: Vec<u64>) -> u64 {
    let resp = api_client(build_api_route())
        .post("/api/v1/users")
        .header("Authorization", format!("Bearer {token}"))
        .body_json(&CreateUserPayload {
            username: username.into(),
            email: format!("{username}@example.com"),
            password: PASSWORD.into(),
            global_roles,
            account_access_map: Default::default(),
        })
        .send()
        .await;
    resp.assert_status_is_ok();
    resp.json().await.value().deserialize::<IdView>().id
}

async fn scrape(token: Option<&str>) -> poem::test::TestResponse {
    let cli = api_client(build_api_route());
    let req = cli.get("/api/v1/metrics");
    match token {
        Some(token) => req.header("Authorization", format!("Bearer {token}")),
        None => req,
    }
    .send()
    .await
}

#[tokio::test]
async fn metrics_requires_a_token() {
    setup().await;
    let resp = scrape(None).await;
    assert_eq!(resp.0.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn metrics_forbidden_without_system_metrics() {
    setup().await;
    let admin = admin_token().await;
    create_user(&admin, "metrics-denied-user", vec![DEFAULT_MEMBER_ROLE_ID]).await;
    let member = login("metrics-denied-user").await;

    let resp = scrape(Some(&member)).await;
    assert_eq!(
        resp.0.status(),
        StatusCode::FORBIDDEN,
        "a member without system:metrics must be rejected"
    );
}

#[tokio::test]
async fn metrics_allowed_for_an_api_token_with_system_metrics() {
    setup().await;
    let admin = admin_token().await;
    let role_id = create_metrics_role(&admin).await;
    let user_id = create_user(&admin, "metrics-scraper-user", vec![role_id]).await;

    let resp = api_client(build_api_route())
        .post("/api/v1/access-token")
        .header("Authorization", format!("Bearer {admin}"))
        .body_json(&CreateTokenPayload {
            name: "prometheus".into(),
            user_id,
        })
        .send()
        .await;
    resp.assert_status_is_ok();
    let api_token = resp.0.into_body().into_string().await.unwrap();

    let resp = scrape(Some(&api_token)).await;
    resp.assert_status_is_ok();
}

#[tokio::test]
async fn metrics_for_admin_is_well_formed_openmetrics() {
    setup().await;
    let admin = admin_token().await;

    let resp = scrape(Some(&admin)).await;
    resp.assert_status_is_ok();
    resp.assert_content_type(bichon_core::metrics::OPENMETRICS_CONTENT_TYPE);
    let body = resp.0.into_body().into_string().await.unwrap();

    for expected in [
        "# TYPE bichon_build_info gauge".to_string(),
        format!(
            "bichon_build_info{{version=\"{}\"}} 1",
            env!("CARGO_PKG_VERSION")
        ),
        "\nbichon_start_time_seconds ".to_string(),
        "bichon_scrape_collector_success{collector=\"accounts\"} 1".to_string(),
    ] {
        assert!(body.contains(&expected), "missing `{expected}` in:\n{body}");
    }

    // The test binary shares one data root between independently initialized
    // test modules, so the index can be unreadable here. Whatever happens, the
    // scrape must still succeed and say whether the index part was collected.
    if body.contains("bichon_scrape_collector_success{collector=\"index\"} 1") {
        assert!(body.contains("\nbichon_emails "), "missing emails:\n{body}");
    } else {
        assert!(
            body.contains("bichon_scrape_collector_success{collector=\"index\"} 0"),
            "index collector not reported:\n{body}"
        );
    }

    let mut types: Vec<&str> = body
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .collect();
    let total = types.len();
    types.sort_unstable();
    types.dedup();
    assert_eq!(total, types.len(), "duplicate # TYPE lines:\n{body}");

    assert!(body.ends_with("# EOF\n"), "missing EOF marker:\n{body}");
    assert_eq!(body.matches("# EOF").count(), 1);
}
