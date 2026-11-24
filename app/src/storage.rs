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
};

// Flat, opaque keys deliberately avoid path traversal and platform-specific aliases.
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
        Router::new()
            .route("/api/objects/{key}", get(read).put(write))
            .layer(DefaultBodyLimit::max(1024 * 1024))
            .with_state(self)
    }

    fn path(&self, key: &str) -> Result<PathBuf, StatusCode> {
        if key.len() != 32
            || !key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        Ok(self.root.join(key))
    }
}

async fn read(
    State(store): State<LocalStorage>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let path = store.path(&key)?;
    let data = tokio::task::spawn_blocking(move || {
        let meta = std::fs::symlink_metadata(&path)?;
        if !meta.is_file() || meta.len() > 1024 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid object"));
        }
        std::fs::read(path)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
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
    State(store): State<LocalStorage>,
    Path(key): Path<String>,
    data: Bytes,
) -> Result<StatusCode, StatusCode> {
    let path = store.path(&key)?;
    tokio::task::spawn_blocking(move || {
        // Publish complete files atomically without replacing an existing object.
        let mut temp =
            tempfile::NamedTempFile::new_in(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        temp.write_all(&data)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path).map_err(|e| e.error)?;
        Ok::<_, io::Error>(())
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
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
}
