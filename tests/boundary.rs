mod support;
use c6_bank::{Client, Error};
use std::time::Duration;
use support::*;

#[tokio::test]
async fn rejects_streamed_resource_overflow_without_replay_or_sensitive_errors() {
    let server = Server::new(|request| {
        if request.starts_with("POST") {
            return Reply::json(TOKEN);
        }
        let mut reply = Reply::json(padded_json(CHARGE, RESOURCE_LIMIT + 1));
        reply.chunked = true;
        reply
    })
    .await;
    let client = server.builder().build().unwrap();
    let error = client.cancel_due_charge("charge").await.unwrap_err();
    assert_eq!(
        error,
        Error::Decode {
            indeterminate: true
        }
    );
    assert!(!format!("{error:?} {error}").contains("synthetic"));
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(
        client.get_due_charge("charge").await.unwrap_err(),
        Error::Decode {
            indeterminate: false
        }
    );
}

#[tokio::test]
async fn rejects_oauth_overflow_with_received_status_and_retry_header() {
    let server = Server::new(|_| {
        let mut reply = Reply::json(padded_json(TOKEN, AUTH_LIMIT + 1));
        reply.chunked = true;
        reply.retry_after = Some("private-retry-header");
        reply
    })
    .await;
    let error = server
        .builder()
        .build()
        .unwrap()
        .cancel_due_charge("charge")
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(200));
    assert_eq!(error.retry_after(), Some("private-retry-header"));
    assert!(!error.is_indeterminate());
    assert!(!format!("{error:?} {error}").contains("private"));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn declared_oversize_is_rejected_before_waiting_for_body() {
    for auth in [true, false] {
        let server = Server::new(move |request| {
            if request.starts_with("POST") && !auth {
                return Reply::json(TOKEN);
            }
            let mut reply = Reply::json(Vec::new());
            reply.declared = Some(if auth {
                AUTH_LIMIT + 1
            } else {
                RESOURCE_LIMIT + 1
            });
            reply
        })
        .await;
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            server.builder().build().unwrap().get_due_charge("charge"),
        )
        .await;
        let error = result
            .expect("declared cap must reject before receiving body")
            .unwrap_err();
        assert!(!error.is_indeterminate());
        if auth {
            assert_eq!(error.status(), Some(200));
        } else {
            assert!(matches!(error, Error::Decode { .. }));
        }
    }
}

#[tokio::test]
async fn complete_exact_limit_json_succeeds_for_oauth_and_resources() {
    let server = Server::new(|request| {
        let (base, length) = if request.starts_with("POST") {
            (TOKEN, AUTH_LIMIT)
        } else {
            (CHARGE, RESOURCE_LIMIT)
        };
        let mut reply = Reply::json(padded_json(base, length));
        reply.chunked = true;
        reply
    })
    .await;
    assert_eq!(
        server
            .builder()
            .build()
            .unwrap()
            .get_due_charge("charge")
            .await
            .unwrap()
            .txid,
        "charge"
    );
}

// This behavioral regression originally used only the native 100ms attempt
// budget and observed a lock waiter still pending after 300ms. The operation
// deadline must also cover time outside the injected transport's own timeout.
#[tokio::test]
async fn authentication_lock_wait_is_bounded_before_resource_dispatch() {
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let held = gate.clone();
    let server = Server::new(move |_| {
        let mut reply = Reply::json(TOKEN);
        reply.gate = Some(held.clone());
        reply
    })
    .await;
    let client = server
        .builder()
        .timeout(Duration::from_millis(100))
        .operation_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let owner = tokio::spawn({
        let client = client.clone();
        async move { client.get_due_charge("owner").await }
    });
    server.observed().await;
    let result = tokio::time::timeout(
        Duration::from_millis(300),
        client.cancel_due_charge("waiter"),
    )
    .await;
    owner.abort();
    let _ = owner.await;
    let error = result
        .expect("authentication lock wait must have a finite operation budget")
        .unwrap_err();
    assert!(matches!(error, Error::Authentication { status: None, .. }));
    assert!(!error.is_indeterminate());
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.starts_with("POST"))
    );
}

#[tokio::test]
async fn auth_and_resource_share_one_deadline_with_phase_correct_uncertainty() {
    for mutation in [false, true] {
        let server = Server::new(|request| {
            let auth = request.starts_with("POST");
            let mut reply = Reply::json(if auth { TOKEN } else { CHARGE });
            reply.before_headers = Duration::from_millis(if auth { 90 } else { 140 });
            reply
        })
        .await;
        let client = server
            .builder()
            .operation_timeout(Duration::from_millis(190))
            .build()
            .unwrap();
        let result = if mutation {
            client.cancel_due_charge("charge").await
        } else {
            client.get_due_charge("charge").await
        };
        assert_eq!(
            result.unwrap_err(),
            Error::Transport {
                indeterminate: mutation
            }
        );
        assert_eq!(server.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn slow_success_body_uses_the_original_deadline_for_get_and_mutation() {
    for mutation in [false, true] {
        let server = Server::new(|request| {
            if request.starts_with("POST") {
                let mut reply = Reply::json(TOKEN);
                reply.before_headers = Duration::from_millis(80);
                return reply;
            }
            let mut reply = Reply::json(padded_json(CHARGE, 24_000));
            reply.chunked = true;
            reply.chunk_delay = Duration::from_millis(50);
            reply
        })
        .await;
        let client = server
            .builder()
            .operation_timeout(Duration::from_millis(190))
            .build()
            .unwrap();
        let result = if mutation {
            client.cancel_due_charge("charge").await
        } else {
            client.get_due_charge("charge").await
        };
        assert_eq!(
            result.unwrap_err(),
            Error::Decode {
                indeterminate: mutation
            }
        );
        assert_eq!(server.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn cancelling_auth_owner_and_waiter_releases_lock_for_later_operations() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let held = gate.clone();
    let server = Server::new(move |request| {
        if !request.starts_with("POST") {
            return Reply::json(CHARGE);
        }
        let mut reply = Reply::json(TOKEN);
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            reply.gate = Some(held.clone());
        }
        reply
    })
    .await;
    let client = server
        .builder()
        .operation_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let owner = tokio::spawn({
        let client = client.clone();
        async move { client.get_due_charge("owner").await }
    });
    server.observed().await;
    let waiter = tokio::spawn({
        let client = client.clone();
        async move { client.get_due_charge("waiter").await }
    });
    tokio::task::yield_now().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    assert_eq!(client.get_due_charge("later").await.unwrap().txid, "charge");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.iter().filter(|r| !r.starts_with("POST")).count(),
        1
    );
    assert!(
        requests
            .last()
            .unwrap()
            .starts_with("GET /v2/pix/cobv/later ")
    );
}

#[tokio::test]
async fn malformed_oauth_success_preserves_real_headers_and_never_dispatches() {
    let server = Server::new(|_| {
        let mut reply = Reply::json("{synthetic-secret-malformed");
        reply.retry_after = Some("private-retry-header");
        reply
    })
    .await;
    let error = server
        .builder()
        .build()
        .unwrap()
        .cancel_due_charge("charge")
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(200));
    assert_eq!(error.retry_after(), Some("private-retry-header"));
    assert!(!error.is_indeterminate());
    assert!(!format!("{error:?} {error}").contains("private"));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn invalid_duration_configuration_is_rejected_before_io() {
    let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    for duration in [Duration::ZERO, Duration::MAX] {
        let client = Client::builder("id", "secret")
            .http_client(reqwest::Client::new())
            .operation_timeout(duration)
            .build();
        assert!(matches!(client, Err(Error::Configuration)));
        let client = Client::builder("id", "secret")
            .identity_pem(
                identity.cert.pem().as_bytes(),
                identity.signing_key.serialize_pem().as_bytes(),
            )
            .unwrap()
            .timeout(duration)
            .build();
        assert!(matches!(client, Err(Error::Configuration)));
        assert!(
            Client::builder("id", "secret")
                .http_client(reqwest::Client::new())
                .timeout(duration)
                .build()
                .is_ok(),
            "injected client retains its own attempt policies"
        );
    }
}

#[tokio::test]
async fn known_401_survives_invalidation_deadline_and_new_same_text_token_is_retained() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for hold_refresh in [false, true] {
        let auths = Arc::new(AtomicUsize::new(0));
        let count = auths.clone();
        let old_reply = Arc::new(tokio::sync::Semaphore::new(0));
        let old_gate = old_reply.clone();
        let refresh_reply = Arc::new(tokio::sync::Semaphore::new(0));
        let refresh_gate = refresh_reply.clone();
        let server = Server::new(move |request| {
            if request.starts_with("POST") {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Reply::json(
                        r#"{"access_token":"synthetic-secret-token","expires_in":0}"#,
                    );
                }
                let mut reply = Reply::json(TOKEN);
                reply.gate = Some(refresh_gate.clone());
                return reply;
            }
            let mut reply = Reply::json(CHARGE);
            if request.starts_with("GET /v2/pix/cobv/old ") {
                reply.status = 401;
                reply.retry_after = Some("private-401-retry");
                reply.gate = Some(old_gate.clone());
            }
            reply
        })
        .await;
        let client = server
            .builder()
            .operation_timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let old = tokio::spawn({
            let client = client.clone();
            async move { client.get_due_charge("old").await }
        });
        server.observed().await; // initial OAuth with immediate expiry
        server.observed().await; // old token's resource request, held before 401
        if hold_refresh {
            tokio::time::sleep(Duration::from_millis(180)).await;
        }
        let refresh = tokio::spawn({
            let client = client.clone();
            async move { client.get_due_charge("refresh").await }
        });
        server.observed().await; // refresh owns token mutex; headers still withheld
        if !hold_refresh {
            refresh_reply.add_permits(1);
            server.observed().await; // refreshed token reached resource dispatch
        }
        old_reply.add_permits(1);
        let error = tokio::time::timeout(Duration::from_millis(800), old)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.status(), Some(401));
        assert_eq!(error.retry_after(), Some("private-401-retry"));
        assert!(!error.is_indeterminate());
        if hold_refresh {
            refresh_reply.add_permits(1);
        }
        refresh.await.unwrap().unwrap();
        client.get_due_charge("later").await.unwrap();
        assert_eq!(
            auths.load(Ordering::SeqCst),
            2,
            "old 401 must not erase a newer token with the same text"
        );
        assert_eq!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.starts_with("GET /v2/pix/cobv/old "))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn oauth_body_deadline_keeps_received_status_and_never_sends_resource() {
    let server = Server::new(|_| {
        let mut reply = Reply::json(padded_json(TOKEN, 32_000));
        reply.chunked = true;
        reply.chunk_delay = Duration::from_millis(60);
        reply.retry_after = Some("private-auth-retry");
        reply
    })
    .await;
    let client = server
        .builder()
        .operation_timeout(Duration::from_millis(150))
        .build()
        .unwrap();
    let error = client.cancel_due_charge("charge").await.unwrap_err();
    assert_eq!(
        error,
        Error::Authentication {
            status: Some(200),
            retry_after: Some("private-auth-retry".into())
        }
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn status_only_success_and_http_errors_do_not_wait_for_body() {
    for status in [200, 429] {
        let server = Server::new(move |request| {
            if request.starts_with("POST") {
                return Reply::json(TOKEN);
            }
            let mut reply = Reply::json(Vec::new());
            reply.declared = Some(RESOURCE_LIMIT + 1);
            reply.status = status;
            reply.retry_after = Some("private-http-retry");
            reply
        })
        .await;
        let client = server.builder().build().unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            client.put_webhook("key", "https://example.test/hook"),
        )
        .await
        .unwrap();
        if status == 200 {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.status(), Some(429));
            assert_eq!(error.retry_after(), Some("private-http-retry"));
            assert!(!error.is_indeterminate());
        }
        assert_eq!(server.requests.lock().unwrap().len(), 2);
    }
}
