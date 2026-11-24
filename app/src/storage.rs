use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

const MAX_OBJECT_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct ObjectKey(String);

impl ObjectKey {
    fn parse(value: String) -> Result<Self, StatusCode> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// Synchronous operations are run on Tokio's blocking pool by the HTTP adapter.
pub trait ObjectStore: Send + Sync {
    fn read(&self, key: &ObjectKey) -> io::Result<Vec<u8>>;
    fn write(&self, key: &ObjectKey, data: &[u8]) -> io::Result<()>;
}

pub fn routes(store: Arc<dyn ObjectStore>) -> Router {
    Router::new()
        .route("/api/objects/{key}", get(read).put(write))
        .layer(DefaultBodyLimit::max(MAX_OBJECT_BYTES))
        .with_state(store)
}

#[derive(Clone)]
pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub fn routes(self) -> Router {
        routes(Arc::new(self))
    }

    fn path(&self, key: &ObjectKey) -> PathBuf {
        self.root.join(key.as_str())
    }
}

impl ObjectStore for LocalStorage {
    fn read(&self, key: &ObjectKey) -> io::Result<Vec<u8>> {
        let path = self.path(key);
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() > MAX_OBJECT_BYTES as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid object"));
        }
        std::fs::read(path)
    }

    fn write(&self, key: &ObjectKey, data: &[u8]) -> io::Result<()> {
        if data.len() > MAX_OBJECT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Object too large",
            ));
        }
        let path = self.path(key);
        let mut temp =
            tempfile::NamedTempFile::new_in(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        temp.write_all(data)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path).map_err(|error| error.error)?;
        Ok(())
    }
}

fn storage_error(error: io::Error) -> StatusCode {
    match error.kind() {
        io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        _ => {
            tracing::error!(error = %error, "Object storage operation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn read(
    State(store): State<Arc<dyn ObjectStore>>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let key = ObjectKey::parse(key)?;
    let data = tokio::task::spawn_blocking(move || store.read(&key))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(storage_error)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        data,
    ))
}

async fn write(
    State(store): State<Arc<dyn ObjectStore>>,
    Path(key): Path<String>,
    data: Bytes,
) -> Result<StatusCode, StatusCode> {
    let key = ObjectKey::parse(key)?;
    tokio::task::spawn_blocking(move || store.write(&key, &data))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(storage_error)?;
    Ok(StatusCode::CREATED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    struct UnavailableStore;

    impl ObjectStore for UnavailableStore {
        fn read(&self, _key: &ObjectKey) -> io::Result<Vec<u8>> {
            Err(io::Error::other("provider unavailable"))
        }

        fn write(&self, _key: &ObjectKey, _data: &[u8]) -> io::Result<()> {
            Err(io::Error::other("provider unavailable"))
        }
    }

    #[tokio::test]
    async fn immutable_objects_and_invalid_keys() {
        let dir = tempfile::tempdir().unwrap();
        let app = LocalStorage::open(dir.path().to_owned()).unwrap().routes();
        let uri = "/api/objects/0123456789abcdef0123456789abcdef";
        let request = |method, path, body| {
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::from(body))
                .unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(request("PUT", uri, "data"))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            app.clone()
                .oneshot(request("PUT", uri, "replacement"))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        let result = app.clone().oneshot(request("GET", uri, "")).await.unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        assert_eq!(to_bytes(result.into_body(), 1024).await.unwrap(), "data");
        assert_eq!(
            app.oneshot(request("PUT", "/api/objects/invalid", "data"))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn local_objects_survive_store_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let uri = "/api/objects/0123456789abcdef0123456789abcdef";
        let store = LocalStorage::open(dir.path().to_owned()).unwrap();
        let app = store.routes();
        let request = Request::builder()
            .method("PUT")
            .uri(uri)
            .body(Body::from("durable"))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::CREATED
        );

        let reopened = LocalStorage::open(dir.path().to_owned()).unwrap().routes();
        let response = reopened
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 100).await.unwrap(),
            "durable"
        );
    }

    #[tokio::test]
    async fn unavailable_provider_fails_without_local_fallback() {
        let app = routes(Arc::new(UnavailableStore));
        let uri = "/api/objects/0123456789abcdef0123456789abcdef";
        for method in ["GET", "PUT"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(Body::from("content"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    #[tokio::test]
    async fn oversized_objects_are_rejected_without_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let app = LocalStorage::open(dir.path().to_owned()).unwrap().routes();
        let uri = "/api/objects/0123456789abcdef0123456789abcdef";
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(uri)
                    .body(Body::from(vec![42; MAX_OBJECT_BYTES + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
