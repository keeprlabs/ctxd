//! End-to-end test of the OAuth2 device-code flow against a wiremock
//! server.

use std::time::Duration;

use ctxd_adapter_gmail::oauth::{
    poll_for_tokens, request_device_code, AuthorizedTokens, OAuthConfig,
};
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn device_flow_persists_refresh_token() {
    let server = MockServer::start().await;

    // Step 1: device-code endpoint returns a code with interval=1s.
    Mock::given(method("POST"))
        .and(path("/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "DEVICE_CODE_VALUE",
            "user_code": "USER-CODE",
            "verification_url": "https://example.com/device",
            "expires_in": 600,
            "interval": 1
        })))
        .mount(&server)
        .await;

    // Step 2: token endpoint — first 2 polls return authorization_pending,
    // third returns the real token.
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=urn"))
        .respond_with(ResponseTemplate::new(428).set_body_json(json!({
            "error": "authorization_pending"
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=urn"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "AT-1",
            "refresh_token": "RT-1",
            "expires_in": 3600,
            "token_type": "Bearer",
            "scope": "https://www.googleapis.com/auth/gmail.readonly"
        })))
        .mount(&server)
        .await;

    let config = OAuthConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        scope: "https://www.googleapis.com/auth/gmail.readonly".into(),
        device_code_url: format!("{}/device/code", server.uri()),
        token_url: format!("{}/token", server.uri()),
    };

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client");

    let code = request_device_code(&http, &config)
        .await
        .expect("device code");
    assert_eq!(code.user_code, "USER-CODE");
    assert_eq!(code.interval, 1);

    let tokens: AuthorizedTokens = poll_for_tokens(&http, &config, &code).await.expect("poll");
    assert_eq!(tokens.refresh_token, "RT-1");
    assert_eq!(tokens.access_token, "AT-1");

    // Now exercise persistence — encrypt + write + read + decrypt.
    let dir = tempfile::tempdir().expect("tmpdir");
    let key_path = dir.path().join("gmail.key");
    let token_path = dir.path().join("gmail.token.enc");

    let key = ctxd_adapter_gmail::crypto::load_or_create_master_key(&key_path)
        .await
        .expect("master key");
    let blob = ctxd_adapter_gmail::crypto::encrypt(&key, tokens.refresh_token.as_bytes())
        .expect("encrypt");
    ctxd_adapter_gmail::crypto::write_secret_file(&token_path, &blob)
        .await
        .expect("write");

    let recovered_blob = tokio::fs::read(&token_path).await.expect("read");
    let plaintext = ctxd_adapter_gmail::crypto::decrypt(&key, &recovered_blob).expect("decrypt");
    assert_eq!(plaintext, b"RT-1");

    // The encrypted blob must NOT contain the plaintext token bytes.
    assert!(
        !blob.windows(4).any(|w| w == b"RT-1"),
        "ciphertext must not leak the plaintext token"
    );
}
