use axum::{
    Router,
    body::Bytes,
    extract::DefaultBodyLimit,
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use launcher_storage::{
    S3CompatibleStorage, S3CompatibleStorageConfig, StorageError, StorageProvider,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[tokio::test]
async fn s3_provider_multipart_retry_duplicate_verification_and_presigning() {
    let (endpoint, state, server) = spawn_mock_s3().await;
    state.fail_part_requests.store(1, Ordering::Release);
    let mut config = test_config(&endpoint);
    config.multipart_threshold_bytes = 5 * 1024 * 1024;
    config.multipart_part_bytes = 5 * 1024 * 1024;
    let storage = S3CompatibleStorage::new(config).unwrap();
    assert!(storage.health_check().await.is_ok());
    let bytes = vec![b'x'; 6 * 1024 * 1024];
    let hash = blake3::hash(&bytes).to_hex().to_string();

    if let Err(error) = storage.put_encoded(&hash, &bytes).await {
        panic!(
            "multipart upload failed: {error:?}; requests={:?}",
            state.requests.lock().await
        );
    }
    assert!(state.objects.lock().await.contains_key(&object_key(&hash)));
    assert!(
        state
            .requests
            .lock()
            .await
            .iter()
            .any(|request| request.contains("partNumber") && request.contains("\"1\""))
    );

    let request_count = state.requests.lock().await.len();
    storage.put_encoded(&hash, &bytes).await.unwrap();
    assert_eq!(state.requests.lock().await.len(), request_count + 1);

    let location = storage.download_location(&hash).await.unwrap();
    assert!(location.url.contains("X-Amz-Expires=900"));
    assert!(location.expires_at.is_some());

    state.corrupt_next_get.store(true, Ordering::Release);
    let corrupt_hash = blake3::hash(b"corrupt-after-upload").to_hex().to_string();
    let error = storage
        .put_encoded(&corrupt_hash, b"corrupt-after-upload")
        .await
        .expect_err("post-upload verification must reject corrupt content");
    assert!(matches!(error, StorageError::HashMismatch { .. }));

    server.abort();
}

#[tokio::test]
async fn s3_provider_aborts_interrupted_multipart_upload_and_cleans_orphans() {
    let (endpoint, state, server) = spawn_mock_s3().await;
    state.fail_part_requests.store(100, Ordering::Release);
    let mut config = test_config(&endpoint);
    config.max_attempts = 1;
    config.multipart_threshold_bytes = 5 * 1024 * 1024;
    config.multipart_part_bytes = 5 * 1024 * 1024;
    let storage = S3CompatibleStorage::new(config).unwrap();
    let bytes = vec![b'y'; 6 * 1024 * 1024];
    let hash = blake3::hash(&bytes).to_hex().to_string();

    assert!(storage.put_encoded(&hash, &bytes).await.is_err());
    assert!(state.uploads.lock().await.is_empty());

    state.uploads.lock().await.insert(
        "orphan-upload".to_owned(),
        MultipartUpload {
            key: object_key(&hash),
            metadata: hash.clone(),
            parts: BTreeMap::new(),
        },
    );
    let cleaned = storage.cleanup_orphaned_multipart_uploads().await;
    match cleaned {
        Ok(value) => assert_eq!(value, 1),
        Err(error) => panic!(
            "cleanup failed: {error:?}; requests={:?}",
            state.requests.lock().await
        ),
    }
    assert!(state.uploads.lock().await.is_empty());
    server.abort();
}

#[tokio::test]
async fn s3_provider_reports_unavailable_endpoint_without_reuploading() {
    let mut config = test_config("http://127.0.0.1:9");
    config.max_attempts = 1;
    let storage = S3CompatibleStorage::new(config).unwrap();
    let hash = blake3::hash(b"unavailable").to_hex().to_string();
    let error = storage.read_encoded(&hash).await.unwrap_err();
    assert!(matches!(error, StorageError::Provider(_)));
    assert!(storage.health_check().await.is_err());
}

#[derive(Clone, Default)]
struct MockState {
    objects: Arc<Mutex<HashMap<String, Object>>>,
    uploads: Arc<Mutex<HashMap<String, MultipartUpload>>>,
    requests: Arc<Mutex<Vec<String>>>,
    next_upload_id: Arc<AtomicUsize>,
    fail_part_requests: Arc<AtomicUsize>,
    corrupt_next_get: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Object {
    bytes: Vec<u8>,
    metadata: String,
}

#[derive(Clone)]
struct MultipartUpload {
    key: String,
    metadata: String,
    parts: BTreeMap<i32, Vec<u8>>,
}

async fn spawn_mock_s3() -> (String, MockState, tokio::task::JoinHandle<()>) {
    let state = MockState::default();
    let app = Router::new()
        .route("/{bucket}", any(handle_bucket))
        .route("/{bucket}/", any(handle_bucket))
        .route("/{bucket}/{*key}", any(handle_object))
        .layer(DefaultBodyLimit::disable())
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, state, server)
}

async fn handle_bucket(
    method: Method,
    Path(bucket): Path<String>,
    RawQuery(raw_query): RawQuery,
    State(state): State<MockState>,
) -> Response {
    let raw_query = raw_query.unwrap_or_default();
    state
        .requests
        .lock()
        .await
        .push(format!("{method} /{bucket}?{raw_query}"));
    if method == Method::HEAD {
        return StatusCode::OK.into_response();
    }
    if method == Method::GET && raw_query.split('&').any(|value| value == "uploads") {
        let uploads = state.uploads.lock().await;
        let body = uploads
            .values()
            .map(|upload| {
                format!(
                    "<Upload><Key>{}</Key><UploadId>orphan-upload</UploadId><StorageClass>STANDARD</StorageClass><Initiated>2026-01-01T00:00:00.000Z</Initiated></Upload>",
                    upload.key
                )
            })
            .collect::<String>();
        return xml_response(
            StatusCode::OK,
            format!(
                "<ListMultipartUploadsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{bucket}</Bucket><KeyMarker></KeyMarker><UploadIdMarker></UploadIdMarker><NextKeyMarker></NextKeyMarker><NextUploadIdMarker></NextUploadIdMarker><MaxUploads>1000</MaxUploads><IsTruncated>false</IsTruncated>{body}</ListMultipartUploadsResult>"
            ),
        );
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn handle_object(
    method: Method,
    Path((_bucket, key)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<MockState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    state
        .requests
        .lock()
        .await
        .push(format!("{method} {key}?{query:?}"));
    if method == Method::HEAD {
        let objects = state.objects.lock().await;
        return objects
            .get(&key)
            .map(|object| {
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-length", object.bytes.len().to_string())
                    .header("x-amz-meta-blake3", object.metadata.clone())
                    .body(axum::body::Body::empty())
                    .unwrap()
            })
            .unwrap_or_else(|| {
                xml_response(
                    StatusCode::NOT_FOUND,
                    "<Error><Code>NoSuchKey</Code></Error>".to_owned(),
                )
            });
    }
    if method == Method::GET {
        let mut objects = state.objects.lock().await;
        let Some(object) = objects.get_mut(&key) else {
            return xml_response(
                StatusCode::NOT_FOUND,
                "<Error><Code>NoSuchKey</Code></Error>".to_owned(),
            );
        };
        let mut bytes = object.bytes.clone();
        if state.corrupt_next_get.swap(false, Ordering::AcqRel) && !bytes.is_empty() {
            bytes[0] ^= 0xff;
        }
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-length", bytes.len().to_string())
            .body(axum::body::Body::from(bytes))
            .unwrap();
    }
    if method == Method::POST && query.contains_key("uploads") {
        let id = format!(
            "upload-{}",
            state.next_upload_id.fetch_add(1, Ordering::AcqRel)
        );
        let metadata = headers
            .get("x-amz-meta-blake3")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        state.uploads.lock().await.insert(
            id.clone(),
            MultipartUpload {
                key: key.clone(),
                metadata,
                parts: BTreeMap::new(),
            },
        );
        return xml_response(
            StatusCode::OK,
            format!(
                "<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>launcher</Bucket><Key>{key}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
            ),
        );
    }
    if method == Method::POST && query.contains_key("uploadId") {
        let upload_id = query.get("uploadId").cloned().unwrap_or_default();
        let Some(upload) = state.uploads.lock().await.remove(&upload_id) else {
            return xml_response(
                StatusCode::NOT_FOUND,
                "<Error><Code>NoSuchUpload</Code></Error>".to_owned(),
            );
        };
        let bytes = upload.parts.values().flatten().copied().collect::<Vec<_>>();
        state.objects.lock().await.insert(
            upload.key.clone(),
            Object {
                bytes,
                metadata: upload.metadata,
            },
        );
        return xml_response(
            StatusCode::OK,
            format!(
                "<CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Location>http://mock/{key}</Location><Bucket>launcher</Bucket><Key>{key}</Key><ETag>\"complete\"</ETag></CompleteMultipartUploadResult>"
            ),
        );
    }
    if method == Method::PUT && query.contains_key("partNumber") {
        if state
            .fail_part_requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                if remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            })
            .is_ok()
        {
            return xml_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "<Error><Code>SlowDown</Code></Error>".to_owned(),
            );
        }
        let upload_id = query.get("uploadId").cloned().unwrap_or_default();
        let part_number = query
            .get("partNumber")
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or_default();
        let mut uploads = state.uploads.lock().await;
        let Some(upload) = uploads.get_mut(&upload_id) else {
            return xml_response(
                StatusCode::NOT_FOUND,
                "<Error><Code>NoSuchUpload</Code></Error>".to_owned(),
            );
        };
        upload.parts.insert(part_number, body.to_vec());
        return Response::builder()
            .status(StatusCode::OK)
            .header("etag", format!("\"part-{part_number}\""))
            .body(axum::body::Body::empty())
            .unwrap();
    }
    if method == Method::DELETE && query.contains_key("uploadId") {
        let upload_id = query.get("uploadId").cloned().unwrap_or_default();
        state.uploads.lock().await.remove(&upload_id);
        return StatusCode::NO_CONTENT.into_response();
    }
    if method == Method::DELETE {
        state.objects.lock().await.remove(&key);
        return StatusCode::NO_CONTENT.into_response();
    }
    if method == Method::PUT {
        let metadata = headers
            .get("x-amz-meta-blake3")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        state.objects.lock().await.insert(
            key,
            Object {
                bytes: body.to_vec(),
                metadata,
            },
        );
        return Response::builder()
            .status(StatusCode::OK)
            .header("etag", "\"single\"")
            .body(axum::body::Body::empty())
            .unwrap();
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

fn xml_response(status: StatusCode, body: String) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/xml")
        .body(axum::body::Body::from(body))
        .unwrap()
}

fn test_config(endpoint: &str) -> S3CompatibleStorageConfig {
    S3CompatibleStorageConfig {
        endpoint: endpoint.to_owned(),
        region: "us-east-1".to_owned(),
        bucket: "launcher".to_owned(),
        access_key: "access".to_owned(),
        secret_key: "secret".to_owned(),
        session_token: None,
        public_base_url: None,
        presign_ttl: Duration::from_secs(900),
        multipart_threshold_bytes: 8 * 1024 * 1024,
        multipart_part_bytes: 8 * 1024 * 1024,
        orphan_multipart_max_age: Duration::from_secs(86_400),
        max_attempts: 3,
        max_concurrent_requests: 2,
        force_path_style: true,
    }
}

fn object_key(hash: &str) -> String {
    format!("chunks/encoded/{hash}.bin")
}
