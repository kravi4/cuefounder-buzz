//! Real PostgreSQL with the shipped HTTP/App Attest/NIP-98 admission path.
use super::*;
use crate::{
    apns::{DeliveryAttempt, PushTransport},
    postgres::PostgresAuthorityStore,
    token::TokenKey,
};
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicI64;
use tower::ServiceExt;
use uuid::Uuid;
static NOW: AtomicI64 = AtomicI64::new(0);
const ORIGIN: &str = "https://push.buzz.xyz";
const APP: &str = "TEAM.test.app";
fn now() -> i64 {
    NOW.load(Ordering::SeqCst)
}
struct Transport;
#[async_trait::async_trait]
impl PushTransport for Transport {
    async fn send(&self, _: DeliveryAttempt, endpoint: &str) -> DeliveryOutcome {
        assert_eq!(endpoint, "01020304");
        DeliveryOutcome::Accepted
    }
}
fn assertion(signed: &str, counter: u32) -> String {
    let mut auth = Sha256::digest(APP.as_bytes()).to_vec();
    auth.push(0x40);
    auth.extend(counter.to_be_bytes());
    let mut digest = Sha256::new();
    digest.update(&auth);
    digest.update(Sha256::digest(signed.as_bytes()));
    let signature: Signature = SigningKey::from_slice(&[7; 32])
        .unwrap()
        .sign(&digest.finalize());
    let mut encoded = Vec::new();
    ciborium::into_writer(
        &ciborium::Value::Map(vec![
            ("authenticatorData".into(), ciborium::Value::Bytes(auth)),
            (
                "signature".into(),
                ciborium::Value::Bytes(signature.to_der().as_bytes().to_vec()),
            ),
        ]),
        &mut encoded,
    )
    .unwrap();
    STANDARD.encode(encoded)
}
async fn post(
    app: &Router,
    path: &str,
    mut body: serde_json::Value,
    signed: Option<(&str, u32)>,
) -> (StatusCode, serde_json::Value) {
    if let Some((transcript, counter)) = signed {
        body.as_object_mut().unwrap().remove("audience");
        body["assertion"] = assertion(transcript, counter).into();
    }
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}
async fn delegate_request(
    app: &Router,
    id: Uuid,
    relay: &str,
    expected: i64,
    generation: i64,
    counter: u32,
) -> (StatusCode, serde_json::Value) {
    let t = DelegateTranscript {
        v: 2,
        audience: DELEGATE_AUDIENCE,
        gateway_origin: Some(ORIGIN),
        request_expires_at: Some(now() + 300),
        challenge_id: None,
        challenge: None,
        installation_handle: id,
        endpoint_epoch: 1,
        expected_generation: Some(expected),
        generation,
        relay_pubkey: relay,
        not_before: now(),
        expires_at: now() + 600,
    };
    post(
        app,
        "/v1/delegations",
        serde_json::to_value(&t).unwrap(),
        Some((&transcript("buzz.push.delegate.v2", &t).unwrap(), counter)),
    )
    .await
}
async fn revoke(app: &Router, id: Uuid, relay: &str, generation: i64, counter: u32) -> StatusCode {
    let t = RevokeDelegationTranscript {
        v: 2,
        audience: REVOKE_DELEGATION_AUDIENCE,
        gateway_origin: Some(ORIGIN),
        request_expires_at: Some(now() + 300),
        challenge_id: None,
        challenge: None,
        installation_handle: id,
        relay_pubkey: relay,
        generation,
    };
    post(
        app,
        "/v1/delegations/revoke",
        serde_json::to_value(&t).unwrap(),
        Some((
            &transcript("buzz.push.revoke-delegation.v2", &t).unwrap(),
            counter,
        )),
    )
    .await
    .0
}
fn delivery_body(grant: &str, request: Uuid) -> Vec<u8> {
    serde_json::to_vec(&DeliveryRequest {
        v: 1,
        endpoint_grant: grant.into(),
        request_id: request,
        expires_at: now() + 120,
    })
    .unwrap()
}
fn auth(keys: &nostr::Keys, body: &[u8]) -> String {
    let event = nostr::EventBuilder::new(nostr::Kind::HttpAuth, "")
        .tags([
            nostr::Tag::parse(["u", "https://push.buzz.xyz/v1/deliveries/apns"]).unwrap(),
            nostr::Tag::parse(["method", "POST"]).unwrap(),
            nostr::Tag::parse(["payload", &hex::encode(Sha256::digest(body))]).unwrap(),
        ])
        .sign_with_keys(keys)
        .unwrap();
    format!(
        "Nostr {}",
        STANDARD.encode(serde_json::to_vec(&event).unwrap())
    )
}
async fn delivery(app: &Router, body: Vec<u8>, auth: &str) -> Response {
    app.clone()
        .oneshot(
            Request::post(DELIVERY_PATH)
                .header("Authorization", auth)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn opaque_http_flow_is_atomic_fair_retryable_and_revocable_under_enrollment_flood() {
    NOW.store(chrono::Utc::now().timestamp(), Ordering::SeqCst);
    let url = std::env::var("BUZZ_TEST_DATABASE_URL").expect("isolated local test database");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("protocol_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = schema.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .after_connect(move |connection, _| {
            let query = format!("SET search_path TO {search_path}");
            Box::pin(async move {
                sqlx::raw_sql(sqlx::AssertSqlSafe(query))
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let err = PostgresAuthorityStore::apply_migrations_and_grants(&pool, "")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("runtime database role"), "{err}");
    let authority = Arc::new(PostgresAuthorityStore::new(pool.clone()));
    let keys = nostr::Keys::generate();
    let other = nostr::Keys::generate();
    let relay = keys.public_key().to_hex();
    let other_relay = other.public_key().to_hex();
    let id = Uuid::new_v4();
    let tokens = TokenKeyring::new(vec![TokenKey::new("token", &[2; 32]).unwrap()]).unwrap();
    authority
        .create_installation(
            NewInstallation {
                id,
                app_attest_key_id: vec![3; 32],
                app_attest_public_key: SigningKey::from_slice(&[7; 32])
                    .unwrap()
                    .verifying_key()
                    .to_sec1_point(false)
                    .as_ref()
                    .to_vec(),
                assertion_counter: 0,
                token_ciphertext: tokens.seal(&[1, 2, 3, 4]).unwrap(),
                token_fingerprint: endpoint_fingerprint(&[1, 2, 3, 4]),
                endpoint_epoch: 1,
                expires_at: now() + 600,
            },
            now(),
        )
        .await
        .unwrap();
    let (app, _) = router(AppState {
        grant_keyring: Arc::new(GrantKeyring::new(vec![]).unwrap()),
        issue_opaque_grants: true,
        legacy_grants_until: Some(now() + 600),
        gateway_origin: ORIGIN.into(),
        authority: authority.clone(),
        token_keyring: Arc::new(tokens),
        application: Arc::new(ApplicationRuntime {
            app_attest: Arc::new(
                AppAttestVerifier::new(
                    APP.into(),
                    include_bytes!("../tests/fixtures/apple-app-attestation-root.pem").to_vec(),
                )
                .unwrap(),
            ),
            transport: Arc::new(Transport),
        }),
        max_grant_lifetime_seconds: 600,
        max_installation_lifetime_seconds: 600,
        endpoint_quota_window_seconds: 60,
        endpoint_quota_max_deliveries: 8,
        now,
        accepting: Arc::new(AtomicBool::new(true)),
    });
    let (status, issued) = delegate_request(&app, id, &relay, 0, 1, 1).await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    let grant = issued["endpoint_grant"].as_str().unwrap();
    assert!(grant.starts_with("g2_"));
    let hash = crate::grant::opaque_hash(grant).unwrap();
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT grant_hash FROM push_gateway_delegations WHERE id=$1")
            .bind(crate::grant::opaque_id(&hash))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, hash);
    let body = delivery_body(grant, Uuid::new_v4());
    assert_eq!(
        delivery(&app, body.clone(), &auth(&other, &body))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let random = crate::grant::issue_opaque().unwrap().0;
    let body = delivery_body(&random, Uuid::new_v4());
    assert_eq!(
        delivery(&app, body.clone(), &auth(&keys, &body))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    for _ in 0..2 {
        let body = delivery_body(grant, Uuid::new_v4());
        assert_eq!(
            delivery(&app, body.clone(), &auth(&keys, &body))
                .await
                .status(),
            StatusCode::OK
        );
    }
    let retry_id = Uuid::new_v4();
    let body = delivery_body(grant, retry_id);
    let response = delivery(&app, body.clone(), &auth(&keys, &body)).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!((1..=60).contains(
        &response.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
    ));
    let (status, renewed) = delegate_request(&app, id, &relay, 1, 2, 2).await;
    assert_eq!(status, StatusCode::CREATED, "{renewed}");
    let renewed = renewed["endpoint_grant"].as_str().unwrap();
    let body = delivery_body(grant, Uuid::new_v4());
    assert_eq!(
        delivery(&app, body.clone(), &auth(&keys, &body))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let retry_body = delivery_body(renewed, retry_id);
    let retry_auth = auth(&keys, &retry_body);
    assert_eq!(
        delivery(&app, retry_body.clone(), &retry_auth)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let (status, second) = delegate_request(&app, id, &other_relay, 0, 1, 3).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let body = delivery_body(second["endpoint_grant"].as_str().unwrap(), Uuid::new_v4());
    assert_eq!(
        delivery(&app, body.clone(), &auth(&other, &body))
            .await
            .status(),
        StatusCode::OK
    );
    // Neither replay identifier was consumed by the quota rejection.
    NOW.fetch_add(61, Ordering::SeqCst);
    assert_eq!(
        delivery(&app, retry_body, &retry_auth).await.status(),
        StatusCode::OK
    );
    // Lose a committed issuance response and request a new token; the hash
    // cannot reconstruct the lost capability. Rejected CAS leaves counter intact.
    assert_eq!(
        delegate_request(&app, id, &relay, 2, 4, 4).await.0,
        StatusCode::CREATED
    );
    let (status, conflict) = delegate_request(&app, id, &relay, 2, 5, 5).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["current_generation"], 4);
    assert_eq!(
        authority
            .installation(id, now())
            .await
            .unwrap()
            .assertion_counter,
        4
    );
    let (status, final_grant) = delegate_request(&app, id, &relay, 4, 6, 6).await;
    assert_eq!(status, StatusCode::CREATED, "{final_grant}");
    assert_eq!(revoke(&app, id, &relay, 6, 7).await, StatusCode::OK);
    assert_eq!(
        delegate_request(&app, id, &relay, 6, 8, 8).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(revoke(&app, id, &relay, 6, 9).await, StatusCode::OK);
    let body = delivery_body(
        final_grant["endpoint_grant"].as_str().unwrap(),
        Uuid::new_v4(),
    );
    assert_eq!(
        delivery(&app, body.clone(), &auth(&keys, &body))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    // Freshness, deployment audience and all target parameters are authenticated.
    for (origin, expiry) in [
        (ORIGIN, now() - 1),
        (ORIGIN, now() + 301),
        ("https://other.example", now() + 300),
    ] {
        let t = RevokeDelegationTranscript {
            v: 2,
            audience: REVOKE_DELEGATION_AUDIENCE,
            gateway_origin: Some(origin),
            request_expires_at: Some(expiry),
            challenge_id: None,
            challenge: None,
            installation_handle: id,
            relay_pubkey: &other_relay,
            generation: 1,
        };
        assert_eq!(
            post(
                &app,
                "/v1/delegations/revoke",
                serde_json::to_value(&t).unwrap(),
                Some((
                    &transcript("buzz.push.revoke-delegation.v2", &t).unwrap(),
                    10
                ))
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let t = RevokeDelegationTranscript {
        v: 2,
        audience: REVOKE_DELEGATION_AUDIENCE,
        gateway_origin: Some(ORIGIN),
        request_expires_at: Some(now() + 300),
        challenge_id: None,
        challenge: None,
        installation_handle: id,
        relay_pubkey: &other_relay,
        generation: 1,
    };
    let signed = transcript("buzz.push.revoke-delegation.v2", &t).unwrap();
    let mut tampered = serde_json::to_value(&t).unwrap();
    tampered["generation"] = 2.into();
    assert_eq!(
        post(
            &app,
            "/v1/delegations/revoke",
            tampered,
            Some((&signed, 10))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        authority
            .installation(id, now())
            .await
            .unwrap()
            .assertion_counter,
        9
    );
    for _ in 0..20 {
        assert_eq!(
            post(
                &app,
                "/v1/installations/challenges",
                serde_json::json!({"v":1}),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        post(
            &app,
            "/v1/installations/challenges",
            serde_json::json!({"v":1}),
            None
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(revoke(&app, id, &other_relay, 1, 10).await, StatusCode::OK);
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
}
