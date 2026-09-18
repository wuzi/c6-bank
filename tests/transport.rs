use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    response::Response,
    routing::any,
};
use c6_bank::{Client, DueCharge, DueChargeRequest, PixQuery};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type RecordedRequest = (String, String, String, String);

#[derive(Clone, Default)]
struct Bank {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    auths: Arc<Mutex<usize>>,
    fail: Arc<Mutex<Option<u16>>>,
    auth_fail: Arc<Mutex<bool>>,
    malformed: Arc<Mutex<bool>>,
    delay: Arc<Mutex<bool>>,
}
async fn handler(State(bank): State<Bank>, req: Request) -> Response {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = String::from_utf8(
        to_bytes(req.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    bank.requests
        .lock()
        .unwrap()
        .push((method.clone(), uri.clone(), auth, body));
    if uri == "/v1/auth/" {
        *bank.auths.lock().unwrap() += 1;
        if *bank.auth_fail.lock().unwrap() {
            return Response::builder()
                .status(401)
                .body(Body::from("private-client-secret"))
                .unwrap();
        }
        return Response::new(Body::from(
            json!({"access_token":"secret-token","expires_in":1,"token_type":"Bearer"}).to_string(),
        ));
    }
    let delay = *bank.delay.lock().unwrap();
    if delay {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    if *bank.malformed.lock().unwrap() {
        return Response::new(Body::from("{malformed secret}"));
    }
    if let Some(status) = *bank.fail.lock().unwrap() {
        return Response::builder()
            .status(status)
            .body(Body::from("secret-bank-error private-identity"))
            .unwrap();
    }
    let result = if uri.starts_with("/v2/pix/pix?") {
        json!({"pix":[],"parametros":{"paginacao":{"paginaAtual":2,"itensPorPagina":50,"quantidadeDePaginas":3,"quantidadeTotalDeItens":101}}})
    } else if uri.starts_with("/v2/pix/pix/") {
        json!({"endToEndId":"receipt","valor":"12.01","horario":"2026-01-01T00:00:00Z"})
    } else if uri.starts_with("/v2/pix/webhook/") {
        if method != "GET" {
            return Response::builder().status(204).body(Body::empty()).unwrap();
        }
        json!({"webhookUrl":"https://example.com/pix"})
    } else {
        json!({"txid":"12345678901234567890123456789012","status":"ATIVA","valor":{"original":"12.01"},"pixCopiaECola":"000201"})
    };
    Response::new(Body::from(result.to_string()))
}
async fn setup() -> (Client, Bank) {
    let bank = Bank::default();
    let app = Router::new()
        .fallback(any(handler))
        .with_state(bank.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Client::builder("client + id", "secret&value")
        .http_client(
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(100))
                .retry(reqwest::retry::never())
                .build()
                .unwrap(),
        )
        .base_url(&url)
        .unwrap()
        .build()
        .unwrap();
    (client, bank)
}
const TXID: &str = "12345678901234567890123456789012";
#[tokio::test]
async fn shared_singleflight_token_and_expiry() {
    let (client, bank) = setup().await;
    let mut tasks = vec![];
    for _ in 0..12 {
        let c = client.clone();
        tasks.push(tokio::spawn(async move {
            c.get_due_charge(TXID).await.unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().txid, TXID);
    }
    assert_eq!(*bank.auths.lock().unwrap(), 1);
    tokio::time::sleep(std::time::Duration::from_millis(1050)).await;
    client.get_due_charge(TXID).await.unwrap();
    assert_eq!(*bank.auths.lock().unwrap(), 2);
    let requests = bank.requests.lock().unwrap();
    assert_eq!(requests[0].0, "POST");
    assert_eq!(
        requests[0].3,
        "grant_type=client_credentials&client_id=client+%2B+id&client_secret=secret%26value"
    );
    assert!(
        requests
            .iter()
            .filter(|r| r.1 != "/v1/auth/")
            .all(|r| r.2 == "Bearer secret-token")
    );
}
#[tokio::test]
async fn sends_due_charge_patch_cancel_receipts_and_webhooks() {
    let (client, bank) = setup().await;
    let request:DueChargeRequest=serde_json::from_value(json!({"calendario":{"dataDeVencimento":"2026-10-01","validadeAposVencimento":90},"devedor":{"nome":"Company","cnpj":"12345678000199","logradouro":"Rua 1","cidade":"Recife","uf":"PE","cep":"50000000"},"valor":{"original":"1234567890.12","multa":{"modalidade":2,"valorPerc":"2.00"}},"chave":"key"})).unwrap();
    client.put_due_charge(TXID, &request).await.unwrap();
    client
        .patch_due_charge(TXID, &json!({"valor":{"original":"1.01"}}))
        .await
        .unwrap();
    client.cancel_due_charge(TXID).await.unwrap();
    assert_eq!(client.get_pix("receipt").await.unwrap().valor, "12.01");
    let page = client
        .list_pix(&PixQuery {
            inicio: "2026-01-01T00:00:00Z".into(),
            fim: "2026-01-02T00:00:00Z".into(),
            pagina_atual: 2,
            itens_por_pagina: 50,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        page.parametros
            .unwrap()
            .paginacao
            .unwrap()
            .quantidade_total_de_itens,
        101
    );
    client
        .put_webhook("key+@/x", "https://example.com/pix")
        .await
        .unwrap();
    client.get_webhook("key+@/x").await.unwrap();
    client.delete_webhook("key+@/x").await.unwrap();
    let r = bank.requests.lock().unwrap();
    assert_eq!(r[1].0, "PUT");
    let sent: Value = serde_json::from_str(&r[1].3).unwrap();
    assert_eq!(sent["valor"]["original"], "1234567890.12");
    assert_eq!(sent["devedor"]["cnpj"], "12345678000199");
    assert!(sent["devedor"].get("cpf").is_none());
    assert_eq!(r[2].0, "PATCH");
    assert_eq!(
        serde_json::from_str::<Value>(&r[3].3).unwrap(),
        json!({"status":"REMOVIDA_PELO_USUARIO_RECEBEDOR"})
    );
    assert!(r[5].1.contains("paginacao.paginaAtual=2"));
    assert!(r[5].1.contains("paginacao.itensPorPagina=50"));
    assert!(r[6].1.ends_with("key+@%2Fx"));
    assert_eq!(r[8].0, "DELETE");
}
#[tokio::test]
async fn classifies_uncertain_mutations_without_replay_or_sensitive_errors() {
    let (client, bank) = setup().await;
    *bank.fail.lock().unwrap() = Some(503);
    let error = client.cancel_due_charge(TXID).await.unwrap_err();
    assert!(error.is_indeterminate());
    assert_eq!(error.status(), Some(503));
    assert!(!format!("{error:?} {error}").contains("secret"));
    assert_eq!(bank.requests.lock().unwrap().len(), 2);
    assert!(
        !client
            .get_due_charge(TXID)
            .await
            .unwrap_err()
            .is_indeterminate()
    );
    *bank.fail.lock().unwrap() = Some(404);
    let error = client.cancel_due_charge(TXID).await.unwrap_err();
    assert!(error.is_not_found());
    assert!(!error.is_indeterminate());
}
#[test]
fn accepts_minimal_charge_but_requires_authoritative_identity() {
    let charge: DueCharge =
        serde_json::from_value(json!({"txid":TXID,"status":"FUTURE_STATUS"})).unwrap();
    assert!(charge.pix.is_empty());
    assert!(charge.pix_copia_e_cola.is_none());
    assert!(serde_json::from_value::<DueCharge>(json!({"status":"ATIVA"})).is_err());
}

#[tokio::test]
async fn timeout_and_invalid_success_are_uncertain_only_for_mutations() {
    let (client, bank) = setup().await;
    *bank.delay.lock().unwrap() = true;
    assert!(
        client
            .cancel_due_charge(TXID)
            .await
            .unwrap_err()
            .is_indeterminate()
    );
    assert_eq!(bank.requests.lock().unwrap().len(), 2);
    *bank.delay.lock().unwrap() = false;
    *bank.malformed.lock().unwrap() = true;
    assert!(
        client
            .cancel_due_charge(TXID)
            .await
            .unwrap_err()
            .is_indeterminate()
    );
    assert!(
        !client
            .get_due_charge(TXID)
            .await
            .unwrap_err()
            .is_indeterminate()
    );
}
#[tokio::test]
async fn authentication_failure_is_definitive_and_never_sends_mutation() {
    let (client, bank) = setup().await;
    *bank.auth_fail.lock().unwrap() = true;
    let error = client.cancel_due_charge(TXID).await.unwrap_err();
    assert!(!error.is_indeterminate());
    assert_eq!(error.status(), Some(401));
    assert_eq!(bank.requests.lock().unwrap().len(), 1);
    assert!(!format!("{error:?} {error}").contains("private"));
}
#[tokio::test]
async fn unauthorized_invalidates_cached_token_without_replaying_mutation() {
    let (client, bank) = setup().await;
    client.get_due_charge(TXID).await.unwrap();
    *bank.fail.lock().unwrap() = Some(401);
    assert_eq!(
        client.cancel_due_charge(TXID).await.unwrap_err().status(),
        Some(401)
    );
    assert_eq!(bank.requests.lock().unwrap().len(), 3);
    *bank.fail.lock().unwrap() = None;
    client.get_due_charge(TXID).await.unwrap();
    assert_eq!(*bank.auths.lock().unwrap(), 2);
}
#[test]
fn response_preserves_unknown_fields_and_optional_receipt_metadata() {
    let charge:DueCharge=serde_json::from_value(json!({"txid":TXID,"status":"CONCLUIDA","devedor":{"cpf":"12345678909"},"pix":[{"endToEndId":"e2e","valor":"10.01","horario":"2026-09-18T12:00:00Z","componentesValor":{"original":{"valor":"10.00"},"juros":{"valor":"0.01"}},"devolucoes":[]}]})).unwrap();
    assert_eq!(charge.extra["devedor"]["cpf"], "12345678909");
    assert_eq!(
        charge.pix[0].componentes_valor.as_ref().unwrap()["original"]["valor"],
        "10.00"
    );
    assert_eq!(charge.pix[0].extra["devolucoes"], json!([]));
    assert!(
        serde_json::from_value::<c6_bank::Pix>(
            json!({"endToEndId":"e2e","valor":10.01,"horario":"now"})
        )
        .is_err()
    );
}

#[test]
fn oauth_not_found_is_not_authoritative_resource_absence() {
    let error = c6_bank::Error::Authentication { status: Some(404) };
    assert!(!error.is_not_found());
    assert!(
        c6_bank::Error::Http {
            status: 404,
            indeterminate: false
        }
        .is_not_found()
    );
}
