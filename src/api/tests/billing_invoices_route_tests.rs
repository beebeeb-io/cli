use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use super::ApiClient;

async fn spawn_billing_invoices_mock() -> String {
    let app = Router::new()
        .route(
            "/api/v1/billing/invoices",
            get(|| async {
                // Exact live-server field shape (billing.rs::invoices) —
                // `number` (not `invoice_number`), `amount_gross_cents`
                // (not `amount_paid`), ISO date strings (not epoch
                // seconds). The server sends no provider-configuration
                // flag in the envelope.
                Json(json!({
                    "invoices": [{
                        "id": "11111111-1111-1111-1111-111111111111",
                        "number": "BB-2026-0001",
                        "invoice_date": "2026-01-15",
                        "period_start": "2026-01-01",
                        "period_end": "2026-01-31",
                        "currency": "eur",
                        "amount_net_cents": 999,
                        "vat_rate_bps": 2100,
                        "vat_amount_cents": 210,
                        "amount_gross_cents": 1209,
                        "vat_treatment": "domestic",
                        "description": "Pro plan",
                        "status": "paid",
                        "pdf_url": "/api/v1/billing/invoices/11111111-1111-1111-1111-111111111111/pdf",
                    }],
                }))
            }),
        )
        .route(
            "/api/v1/billing/invoices/:id/pdf",
            get(|Path(id): Path<String>| async move {
                assert_eq!(id, "11111111-1111-1111-1111-111111111111");
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/pdf")],
                    b"%PDF-1.4 fake invoice pdf".to_vec(),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn get_billing_invoices_reads_the_live_field_shape() {
    let api = ApiClient::new_for_test(spawn_billing_invoices_mock().await);
    let body = api.get_billing_invoices().await.expect("mock request should succeed");
    let invoices = body.get("invoices").and_then(|v| v.as_array()).expect("invoices array");
    assert_eq!(invoices.len(), 1);
    let inv = &invoices[0];
    assert_eq!(inv.get("number").and_then(|v| v.as_str()), Some("BB-2026-0001"));
    assert_eq!(inv.get("amount_gross_cents").and_then(|v| v.as_i64()), Some(1209));
    assert_eq!(inv.get("invoice_date").and_then(|v| v.as_str()), Some("2026-01-15"));
    let envelope = body.as_object().expect("invoices envelope object");
    assert_eq!(
        envelope.keys().collect::<Vec<_>>(),
        vec!["invoices"],
        "the envelope is {{ invoices: [...] }} — the client must parse a body with no \
         provider-configuration flag: {body}"
    );
}

#[tokio::test]
async fn download_invoice_pdf_returns_the_raw_bytes() {
    let api = ApiClient::new_for_test(spawn_billing_invoices_mock().await);
    let bytes = api
        .download_invoice_pdf("11111111-1111-1111-1111-111111111111")
        .await
        .expect("mock pdf download should succeed");
    assert_eq!(bytes, b"%PDF-1.4 fake invoice pdf".to_vec());
}

#[tokio::test]
async fn download_invoice_pdf_surfaces_a_404_for_an_unknown_id() {
    let app = Router::new().route(
        "/api/v1/billing/invoices/:id/pdf",
        get(|| async { StatusCode::NOT_FOUND }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let api = ApiClient::new_for_test(format!("http://{addr}"));
    let err = api
        .download_invoice_pdf("00000000-0000-0000-0000-000000000000")
        .await
        .expect_err("a 404 must surface as an Err, not silently return empty bytes");
    assert!(err.message.contains("404"), "error should mention the status: {err}");
}
