//! Local, deterministic generation; never executes or publishes source code.
use crate::identity::{Role, User};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{StatusCode, header},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{SqlitePool, types::Json as StoredJson};
use std::collections::BTreeMap;

type Error = (StatusCode, &'static str);
const INVALID: Error = (StatusCode::BAD_REQUEST, "Invalid scaffolder parameters");
const MISSING: Error = (StatusCode::NOT_FOUND, "Template or job not found");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    name: String,
    version: String,
    parameters: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct Parameter {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    required: bool,
}

#[derive(sqlx::FromRow)]
struct Definition {
    stack: String,
    parameters: StoredJson<Vec<Parameter>>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    name: String,
    version: String,
    stack: String,
    #[serde(deserialize_with = "unique_files")]
    files: BTreeMap<String, String>,
}

fn unique_files<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique file paths")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut files = BTreeMap::new();
            while let Some((k, v)) = map.next_entry::<String, String>()? {
                if files.insert(k, v).is_some() {
                    return Err(serde::de::Error::custom("Duplicate file path"));
                }
            }
            Ok(files)
        }
    }
    d.deserialize_map(Visitor)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Job {
    id: i64,
    actor_id: i64,
    name: String,
    version: String,
    created_at: i64,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: u32,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    50
}

pub fn routes(pool: SqlitePool) -> Router {
    Router::new()
        .route("/api/scaffolder/dry-run", post(dry_run))
        .route("/api/scaffolder/jobs", get(history).post(create))
        .route("/api/scaffolder/jobs/{id}", get(detail))
        .route("/api/scaffolder/jobs/{id}/artifact", get(download))
        .route("/api/scaffolder/jobs/{id}/archive", get(download_archive))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(pool)
}

fn database_error(_: sqlx::Error) -> Error {
    tracing::error!("Scaffolder database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Scaffolder database operation failed",
    )
}

fn require_writer(user: &User) -> Result<(), Error> {
    if user.role == Role::Viewer {
        return Err((StatusCode::FORBIDDEN, "Member or administrator required"));
    }
    Ok(())
}

fn valid_project_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0].is_ascii_lowercase()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

fn validate_parameters(
    schema: &[Parameter],
    values: &BTreeMap<String, Value>,
) -> Result<(), Error> {
    if schema.len() > 20 || values.len() > 20 {
        return Err(INVALID);
    }
    for key in values.keys() {
        if !schema.iter().any(|field| &field.name == key) {
            return Err(INVALID);
        }
    }
    for field in schema {
        let Some(value) = values.get(&field.name) else {
            if field.required {
                return Err(INVALID);
            }
            continue;
        };
        let valid = match field.kind.as_str() {
            "string" => value.as_str().is_some_and(|s| {
                s.len() <= 2000
                    && !s.chars().any(char::is_control)
                    && (!field.required || !s.trim().is_empty())
            }),
            "boolean" => value.is_boolean(),
            "integer" => value
                .as_i64()
                .is_some_and(|n| (-2147483648..=2147483647).contains(&n)),
            _ => false,
        };
        if !valid {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn render(input: &Input, definition: &Definition) -> Result<Artifact, Error> {
    validate_parameters(&definition.parameters.0, &input.parameters)?;
    let project = match input.parameters.get("project-name") {
        Some(value) => value.as_str().ok_or(INVALID)?,
        None => "starter-project",
    };
    if !valid_project_name(project) {
        return Err(INVALID);
    }
    // Values enter a JSON document via serialization, never executable source or paths.
    let metadata =
        serde_json::to_string_pretty(&json!({"project":project,"parameters":input.parameters}))
            .map_err(|_| INVALID)?;
    let mut files = BTreeMap::from([
        ("panggung.json".into(), metadata),
        ("README.md".into(), "Generated starter. Review source and dependencies before building. No commands have been run.\n".into()),
    ]);
    let starter: &[(&str, &str)] = match definition.stack.as_str() {
        "rust" => &[
            (
                "Cargo.toml",
                "[package]\nname = \"starter\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            (
                "src/main.rs",
                "fn main() { println!(\"Hello from Panggung\"); }\n",
            ),
        ],
        "dotnet" => &[
            (
                "Starter.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>\n",
            ),
            (
                "Program.cs",
                "System.Console.WriteLine(\"Hello from Panggung\");\n",
            ),
        ],
        "python" => &[(
            "main.py",
            "def main():\n    print(\"Hello from Panggung\")\n\nif __name__ == \"__main__\":\n    main()\n",
        )],
        "node" => &[
            (
                "package.json",
                "{\"name\":\"starter\",\"version\":\"0.1.0\",\"private\":true,\"type\":\"module\"}\n",
            ),
            ("index.js", "console.log('Hello from Panggung');\n"),
        ],
        "react" => &[
            (
                "package.json",
                "{\"name\":\"starter\",\"private\":true,\"version\":\"0.1.0\",\"type\":\"module\",\"scripts\":{\"dev\":\"vite\",\"build\":\"vite build\"},\"dependencies\":{\"react\":\"^19.0.0\",\"react-dom\":\"^19.0.0\"},\"devDependencies\":{\"vite\":\"^7.0.0\"}}\n",
            ),
            (
                "index.html",
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Starter</title></head><body><div id=\"root\"></div><script type=\"module\" src=\"/src/main.jsx\"></script></body></html>\n",
            ),
            (
                "src/main.jsx",
                "import React from 'react';\nimport { createRoot } from 'react-dom/client';\ncreateRoot(document.getElementById('root')).render(<h1>Hello from Panggung</h1>);\n",
            ),
        ],
        _ => return Err(INVALID),
    };
    files.extend(
        starter
            .iter()
            .map(|(path, content)| (path.to_string(), content.to_string())),
    );
    Ok(Artifact {
        name: input.name.clone(),
        version: input.version.clone(),
        stack: definition.stack.clone(),
        files,
    })
}

async fn generate(pool: &SqlitePool, input: &Input) -> Result<Artifact, Error> {
    if input.name.len() > 63 || input.version.len() > 29 {
        return Err(INVALID);
    }
    let definition: Definition = sqlx::query_as(
        "SELECT stack, parameters FROM software_template_versions WHERE name = ? AND version = ?",
    )
    .bind(&input.name)
    .bind(&input.version)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .ok_or(MISSING)?;
    render(input, &definition)
}

async fn dry_run(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Json(input): Json<Input>,
) -> Result<Json<Artifact>, Error> {
    require_writer(&user)?;
    generate(&pool, &input).await.map(Json)
}

async fn create(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Json(input): Json<Input>,
) -> Result<(StatusCode, Json<Job>), Error> {
    require_writer(&user)?;
    let artifact = generate(&pool, &input).await?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let job: Job = sqlx::query_as("INSERT INTO scaffolder_jobs (actor_id,name,version,artifact) VALUES (?,?,?,?) RETURNING id,actor_id,name,version,created_at")
        .bind(user.id).bind(&input.name).bind(&input.version).bind(StoredJson(artifact))
        .fetch_one(&mut *transaction).await.map_err(database_error)?;
    sqlx::query("INSERT INTO identity_audit_events (actor_id,action) VALUES (?,?)")
        .bind(user.id)
        .bind(format!("create_scaffolder_job:{}", job.id))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(job)))
}

async fn history(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Job>>, Error> {
    if !(1..=100).contains(&page.limit) {
        return Err(INVALID);
    }
    sqlx::query_as("SELECT id,actor_id,name,version,created_at FROM scaffolder_jobs WHERE actor_id = ? OR ? ORDER BY id DESC LIMIT ? OFFSET ?")
        .bind(user.id).bind(user.role == Role::Admin).bind(page.limit).bind(page.offset)
        .fetch_all(&pool).await.map(Json).map_err(database_error)
}

async fn detail(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Path(id): Path<i64>,
) -> Result<Json<Job>, Error> {
    sqlx::query_as("SELECT id,actor_id,name,version,created_at FROM scaffolder_jobs WHERE id = ? AND (actor_id = ? OR ?)")
        .bind(id).bind(user.id).bind(user.role == Role::Admin).fetch_optional(&pool).await.map_err(database_error)?.map(Json).ok_or(MISSING)
}

async fn download(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Path(id): Path<i64>,
) -> Result<([(header::HeaderName, &'static str); 2], String), Error> {
    let artifact: String = sqlx::query_scalar(
        "SELECT artifact FROM scaffolder_jobs WHERE id = ? AND (actor_id = ? OR ?)",
    )
    .bind(id)
    .bind(user.id)
    .bind(user.role == Role::Admin)
    .fetch_optional(&pool)
    .await
    .map_err(database_error)?
    .ok_or(MISSING)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"panggung-starter.json\"",
            ),
        ],
        artifact,
    ))
}

const ARCHIVE_ERROR: Error = (
    StatusCode::INTERNAL_SERVER_ERROR,
    "Invalid stored starter artifact",
);

fn allowed_paths(stack: &str) -> Option<&'static [&'static str]> {
    match stack {
        "rust" => Some(&["README.md", "panggung.json", "Cargo.toml", "src/main.rs"]),
        "dotnet" => Some(&["README.md", "panggung.json", "Starter.csproj", "Program.cs"]),
        "python" => Some(&["README.md", "panggung.json", "main.py"]),
        "node" => Some(&["README.md", "panggung.json", "package.json", "index.js"]),
        "react" => Some(&[
            "README.md",
            "panggung.json",
            "package.json",
            "index.html",
            "src/main.jsx",
        ]),
        _ => None,
    }
}

/// Publishing and archive generation share the same immutable artifact contract.
pub(crate) fn publication_files(body: &str) -> Result<BTreeMap<String, String>, Error> {
    if body.len() > 512 * 1024 {
        return Err(ARCHIVE_ERROR);
    }
    let artifact: Artifact = serde_json::from_str(body).map_err(|_| ARCHIVE_ERROR)?;
    archive(&artifact)?;
    Ok(artifact.files)
}

fn archive(artifact: &Artifact) -> Result<Vec<u8>, Error> {
    let paths = allowed_paths(&artifact.stack).ok_or(ARCHIVE_ERROR)?;
    if artifact.files.len() != paths.len()
        || !paths.iter().all(|path| artifact.files.contains_key(*path))
    {
        return Err(ARCHIVE_ERROR);
    }
    let mut total = 0usize;
    let mut builder = tar::Builder::new(Vec::new());
    for (path, content) in &artifact.files {
        total = total.checked_add(content.len()).ok_or(ARCHIVE_ERROR)?;
        if total > 256 * 1024 {
            return Err(ARCHIVE_ERROR);
        }
        let mut entry = tar::Header::new_ustar();
        entry.set_entry_type(tar::EntryType::Regular);
        entry.set_size(content.len() as u64);
        entry.set_mode(0o644);
        entry.set_uid(0);
        entry.set_gid(0);
        entry.set_mtime(0);
        entry.set_cksum();
        builder
            .append_data(&mut entry, path, content.as_bytes())
            .map_err(|_| ARCHIVE_ERROR)?;
    }
    builder.into_inner().map_err(|_| ARCHIVE_ERROR)
}

async fn download_archive(
    State(pool): State<SqlitePool>,
    Extension(user): Extension<User>,
    Path(id): Path<i64>,
) -> Result<([(header::HeaderName, &'static str); 3], Vec<u8>), Error> {
    let (_, body) = download(State(pool), Extension(user), Path(id)).await?;
    if body.len() > 512 * 1024 {
        return Err(ARCHIVE_ERROR);
    }
    let artifact: Artifact = serde_json::from_str(&body).map_err(|_| ARCHIVE_ERROR)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"panggung-starter.tar\"",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        archive(&artifact)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema(stack: &str) -> Definition {
        Definition {
            stack: stack.into(),
            parameters: StoredJson(vec![
                Parameter {
                    name: "project-name".into(),
                    kind: "string".into(),
                    required: true,
                },
                Parameter {
                    name: "enabled".into(),
                    kind: "boolean".into(),
                    required: false,
                },
                Parameter {
                    name: "count".into(),
                    kind: "integer".into(),
                    required: false,
                },
            ]),
        }
    }
    fn input() -> Input {
        serde_json::from_value(json!({"name":"starter","version":"1.0.0","parameters":{"project-name":"safe-app","enabled":true,"count":1}})).unwrap()
    }
    #[test]
    fn deterministic_fixed_paths_for_all_stacks() {
        for (stack, path) in [
            ("rust", "src/main.rs"),
            ("dotnet", "Program.cs"),
            ("python", "main.py"),
            ("node", "index.js"),
            ("react", "src/main.jsx"),
        ] {
            let a = render(&input(), &schema(stack)).unwrap();
            assert!(a.files.contains_key(path));
            assert_eq!(a, render(&input(), &schema(stack)).unwrap());
            assert!(
                a.files
                    .keys()
                    .all(|p| !p.starts_with('/') && !p.contains("..") && !p.contains('\\'))
            );
        }
    }
    #[test]
    fn rejects_bad_types_unknown_fields_and_unsafe_names() {
        let mut data = input();
        data.parameters
            .insert("project-name".into(), json!("../escape"));
        assert!(render(&data, &schema("rust")).is_err());
        let mut data = input();
        data.parameters.insert("count".into(), json!(1.5));
        assert!(render(&data, &schema("rust")).is_err());
        let mut data = input();
        data.parameters.insert("command".into(), json!("anything"));
        assert!(render(&data, &schema("rust")).is_err());
        let mut data = input();
        data.parameters.remove("project-name");
        assert!(render(&data, &schema("rust")).is_err());
    }
    #[test]
    fn archives_extract_exact_files_for_all_stacks() {
        for stack in ["rust", "dotnet", "python", "node", "react"] {
            let artifact = render(&input(), &schema(stack)).unwrap();
            let bytes = archive(&artifact).unwrap();
            assert_eq!(bytes, archive(&artifact).unwrap());
            let dir = tempfile::tempdir().unwrap();
            tar::Archive::new(bytes.as_slice())
                .unpack(dir.path())
                .unwrap();
            for (path, content) in &artifact.files {
                assert_eq!(
                    std::fs::read_to_string(dir.path().join(path)).unwrap(),
                    *content
                );
            }
            let mut tampered = artifact;
            tampered.files.insert("../escape".into(), "bad".into());
            assert!(archive(&tampered).is_err());
        }
    }
    #[tokio::test]
    async fn dry_run_history_and_download_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("jobs.db"))
            .await
            .unwrap();
        for (name, role) in [
            ("admin", "admin"),
            ("member", "member"),
            ("other", "member"),
            ("viewer", "viewer"),
        ] {
            sqlx::query("INSERT INTO identity_users (username,password_hash,role) VALUES (?, 'test-only', ?)").bind(name).bind(role).execute(&pool).await.unwrap();
        }
        let actor = |id, role| {
            Extension(User {
                id,
                username: "test".into(),
                role,
            })
        };
        let request = || {
            Json(serde_json::from_value(json!({"name":"rust-service","version":"1.0.0","parameters":{"project-name":"safe-app"}})).unwrap())
        };
        let Json(preview) = dry_run(State(pool.clone()), actor(2, Role::Member), request())
            .await
            .unwrap();
        let Json(empty) = history(
            State(pool.clone()),
            actor(1, Role::Admin),
            Query(Page {
                offset: 0,
                limit: 50,
            }),
        )
        .await
        .unwrap();
        assert!(empty.is_empty());
        assert_eq!(
            create(State(pool.clone()), actor(4, Role::Viewer), request())
                .await
                .unwrap_err()
                .0,
            StatusCode::FORBIDDEN
        );
        let (status, Json(job)) = create(State(pool.clone()), actor(2, Role::Member), request())
            .await
            .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            detail(State(pool.clone()), actor(3, Role::Member), Path(job.id))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            download(State(pool.clone()), actor(3, Role::Member), Path(job.id))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        let (_, body) = download(State(pool.clone()), actor(1, Role::Admin), Path(job.id))
            .await
            .unwrap();
        assert_eq!(serde_json::from_str::<Artifact>(&body).unwrap(), preview);
        assert_eq!(
            download_archive(State(pool.clone()), actor(3, Role::Member), Path(job.id))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        let (_, bytes) = download_archive(State(pool.clone()), actor(1, Role::Admin), Path(job.id))
            .await
            .unwrap();
        assert_eq!(bytes, archive(&preview).unwrap());
        let Json(own) = history(
            State(pool.clone()),
            actor(2, Role::Member),
            Query(Page {
                offset: 0,
                limit: 50,
            }),
        )
        .await
        .unwrap();
        assert_eq!(own.len(), 1);
        let Json(other) = history(
            State(pool.clone()),
            actor(3, Role::Member),
            Query(Page {
                offset: 0,
                limit: 50,
            }),
        )
        .await
        .unwrap();
        assert!(other.is_empty());
        pool.close().await;
        let reopened = crate::database::open(&dir.path().join("jobs.db"))
            .await
            .unwrap();
        let (_, body) = download(State(reopened), actor(2, Role::Member), Path(job.id))
            .await
            .unwrap();
        assert_eq!(serde_json::from_str::<Artifact>(&body).unwrap(), preview);
    }
}
