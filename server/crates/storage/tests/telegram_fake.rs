use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    response::Response,
    routing::any,
};
use launcher_storage::{
    StorageClass, StorageProvider, TelegramColdStorageConfig, TelegramColdStorageProvider,
};
use std::path::PathBuf;
use tokio::net::TcpListener;

#[tokio::test]
async fn fake_telegram_upload_restore_delete_keeps_credentials_out_of_state() {
    let bytes = b"fake telegram pack".to_vec();
    let app = Router::new().fallback(any({
        let bytes = bytes.clone();
        move |request: Request<Body>| {
            let bytes = bytes.clone();
            async move {
                let path = request.uri().path().to_owned();
                if path.ends_with("/getMe") {
                    return json_response(serde_json::json!({"ok":true,"result":{"id":1}}));
                }
                if path.ends_with("/getChat") {
                    return json_response(serde_json::json!({"ok":true,"result":{"id":123}}));
                }
                if path.ends_with("/sendDocument") {
                    return json_response(serde_json::json!({"ok":true,"result":{"message_id":7,"document":{"file_id":"fake-file","file_unique_id":"fake-unique"}}}));
                }
                if path.ends_with("/getFile") {
                    return json_response(serde_json::json!({"ok":true,"result":{"file_path":"documents/fake.pack"}}));
                }
                if path.ends_with("/deleteMessage") {
                    return json_response(serde_json::json!({"ok":true,"result":true}));
                }
                if path.ends_with("/documents/fake.pack") {
                    return Response::builder().status(StatusCode::OK).body(Body::from(bytes)).unwrap();
                }
                Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap()
            }
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let root = std::env::temp_dir().join(format!("launcher-telegram-fake-{}", std::process::id()));
    let state_file = root.join("state.json");
    let config = TelegramColdStorageConfig {
        provider_id: "telegram-fake".to_owned(),
        pool_id: "telegram-fake".to_owned(),
        failure_domain: "telegram-fake-domain".to_owned(),
        bot_token: "do-not-persist-this-token".to_owned(),
        chat_ids: vec![123],
        api_base_url: format!("http://{address}"),
        state_file: PathBuf::from(&state_file),
        max_upload_bytes: 1024 * 1024,
        request_timeout_seconds: 10,
    };
    let provider = TelegramColdStorageProvider::new(config).unwrap();
    provider.health_check().await.unwrap();
    let pack_hash = blake3::hash(&bytes).to_hex().to_string();
    provider.put_pack(&pack_hash, &bytes).await.unwrap();
    assert_eq!(provider.read_pack(&pack_hash).await.unwrap(), bytes);
    for concurrency in [1_usize, 2, 4, 8, 16] {
        let started = tokio::time::Instant::now();
        let mut tasks = Vec::with_capacity(concurrency);
        for _ in 0..concurrency {
            let provider = provider.clone();
            let pack_hash = pack_hash.clone();
            tasks.push(tokio::spawn(
                async move { provider.read_pack(&pack_hash).await },
            ));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().unwrap(), bytes);
        }
        println!(
            "telegram_fake_restore_concurrency={} elapsed_ms={}",
            concurrency,
            started.elapsed().as_millis()
        );
    }
    provider.delete_pack(&pack_hash).await.unwrap();
    let state = tokio::fs::read_to_string(&state_file).await.unwrap();
    assert!(!state.contains("do-not-persist-this-token"));
    assert_eq!(provider.tier(), StorageClass::Cold);

    server.abort();
    let _ = tokio::fs::remove_dir_all(root).await;
}

fn json_response(value: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
