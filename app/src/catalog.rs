use crate::catalog_repository::{CatalogRepository, CatalogState, RepositoryError};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct Component {
    pub name: String,
    pub description: String,
    pub owner: String,
    pub namespace: String,
    pub component_type: Option<String>,
    pub lifecycle: Option<String>,
    pub descriptor: Option<Value>,
}

#[derive(Deserialize)]
pub struct NewComponent {
    name: String,
    description: String,
    owner: String,
}

type ApiError = (StatusCode, &'static str);
const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
const DEFAULT_NAMESPACE: &str = "default";
const MAX_PAGE_SIZE: u32 = 500;

fn default_limit() -> u32 {
    MAX_PAGE_SIZE
}

fn internal(error: RepositoryError) -> ApiError {
    if matches!(error, RepositoryError::Conflict) {
        return (StatusCode::CONFLICT, "Component already exists");
    }
    tracing::error!("Catalog database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database operation failed",
    )
}

#[derive(Deserialize)]
pub struct Filter {
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: u32,
    namespace: Option<String>,
    #[serde(default = "default_limit")]
    limit: u32,
}

pub async fn list(
    State(repository): State<CatalogState>,
    Query(filter): Query<Filter>,
) -> Result<Json<Vec<Component>>, ApiError> {
    if filter.q.len() > 200 {
        return Err((StatusCode::BAD_REQUEST, "Search term too long"));
    }
    if !(1..=MAX_PAGE_SIZE).contains(&filter.limit) {
        return Err((StatusCode::BAD_REQUEST, "Invalid page limit"));
    }
    if let Some(namespace) = &filter.namespace {
        validate_namespace(namespace)?;
    }
    repository
        .list(
            &filter.q,
            filter.namespace.as_deref(),
            filter.offset,
            filter.limit,
        )
        .await
        .map(Json)
        .map_err(internal)
}

pub async fn create(
    State(repository): State<CatalogState>,
    Json(input): Json<NewComponent>,
) -> Result<(StatusCode, Json<Component>), ApiError> {
    let component = repository
        .insert(validate(input)?)
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(component)))
}

fn validate(input: NewComponent) -> Result<Component, ApiError> {
    let name = input.name.trim();
    let owner = input.owner.trim();
    let description = input.description.trim();
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
        || owner.is_empty()
        || owner.len() > 100
        || description.len() > 2000
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid component fields"));
    }
    Ok(Component {
        name: name.into(),
        description: description.into(),
        owner: owner.into(),
        namespace: DEFAULT_NAMESPACE.into(),
        component_type: None,
        lifecycle: None,
        descriptor: None,
    })
}

fn validate_namespace(namespace: &str) -> Result<(), ApiError> {
    if namespace.is_empty()
        || namespace.len() > 63
        || !namespace.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
        || !namespace.as_bytes()[0].is_ascii_alphanumeric()
        || !namespace.as_bytes()[namespace.len() - 1].is_ascii_alphanumeric()
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid namespace"));
    }
    Ok(())
}

pub async fn get_one(
    State(repository): State<CatalogState>,
    Path(name): Path<String>,
) -> Result<Json<Component>, ApiError> {
    get_qualified(State(repository), Path((DEFAULT_NAMESPACE.into(), name))).await
}

pub async fn get_qualified(
    State(repository): State<CatalogState>,
    Path((namespace, name)): Path<(String, String)>,
) -> Result<Json<Component>, ApiError> {
    validate_namespace(&namespace)?;
    repository
        .get(&namespace, &name)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, "Component not found"))
}

pub async fn update(
    State(repository): State<CatalogState>,
    Path(name): Path<String>,
    Json(input): Json<NewComponent>,
) -> Result<Json<Component>, ApiError> {
    update_qualified(
        State(repository),
        Path((DEFAULT_NAMESPACE.into(), name)),
        Json(input),
    )
    .await
}

pub async fn update_qualified(
    State(repository): State<CatalogState>,
    Path((namespace, name)): Path<(String, String)>,
    Json(input): Json<NewComponent>,
) -> Result<Json<Component>, ApiError> {
    validate_namespace(&namespace)?;
    let component = validate(input)?;
    if name != component.name {
        return Err((StatusCode::BAD_REQUEST, "Component names cannot be changed"));
    }
    repository
        .update(&namespace, &name, &component.description, &component.owner)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, "Component not found"))
}

pub async fn remove(
    State(repository): State<CatalogState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    remove_qualified(State(repository), Path((DEFAULT_NAMESPACE.into(), name))).await
}

pub async fn remove_qualified(
    State(repository): State<CatalogState>,
    Path((namespace, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    validate_namespace(&namespace)?;
    if !repository
        .remove(&namespace, &name)
        .await
        .map_err(internal)?
    {
        return Err((StatusCode::NOT_FOUND, "Component not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    api_version: String,
    kind: String,
    metadata: Metadata,
    spec: Spec,
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Deserialize)]
struct Spec {
    owner: String,
    #[serde(rename = "type")]
    component_type: String,
    lifecycle: String,
}

pub async fn import(
    State(repository): State<CatalogState>,
    Json(document): Json<Value>,
) -> Result<(StatusCode, Json<Component>), ApiError> {
    import_document(repository.as_ref(), document).await
}

pub async fn import_yaml(
    State(repository): State<CatalogState>,
    body: String,
) -> Result<(StatusCode, Json<Component>), ApiError> {
    if body.len() > MAX_DESCRIPTOR_BYTES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Descriptor too large"));
    }
    let document = serde_yaml_ng::from_str(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid YAML descriptor"))?;
    import_document(repository.as_ref(), document).await
}

async fn import_document(
    repository: &dyn CatalogRepository,
    document: Value,
) -> Result<(StatusCode, Json<Component>), ApiError> {
    let component = validate_descriptor(document)?;
    Ok((
        StatusCode::CREATED,
        Json(repository.insert(component).await.map_err(internal)?),
    ))
}

fn validate_descriptor(mut document: Value) -> Result<Component, ApiError> {
    let invalid = (StatusCode::BAD_REQUEST, "Invalid component descriptor");
    reject_substitutions(&document, 0)?;
    if document.to_string().len() > MAX_DESCRIPTOR_BYTES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Descriptor too large"));
    }
    let input: Descriptor = serde_json::from_value(document.clone()).map_err(|_| invalid)?;
    if input.api_version != "backstage.io/v1alpha1"
        || input.kind != "Component"
        || input.spec.component_type.trim().is_empty()
        || input.spec.component_type.len() > 100
        || input.spec.lifecycle.trim().is_empty()
        || input.spec.lifecycle.len() > 100
    {
        return Err((StatusCode::BAD_REQUEST, "Unsupported component descriptor"));
    }
    validate_metadata(&document["metadata"])?;
    validate_spec(&document["spec"])?;
    let namespace = input
        .metadata
        .namespace
        .unwrap_or_else(|| DEFAULT_NAMESPACE.into());
    validate_namespace(&namespace)?;
    let mut component = validate(NewComponent {
        name: input.metadata.name,
        description: input.metadata.description,
        owner: input.spec.owner,
    })?;
    component.namespace = namespace;
    component.component_type = Some(input.spec.component_type);
    component.lifecycle = Some(input.spec.lifecycle);
    document["metadata"]["name"] = Value::String(component.name.clone());
    if document["metadata"].get("description").is_some() {
        document["metadata"]["description"] = Value::String(component.description.clone());
    }
    document["spec"]["owner"] = Value::String(component.owner.clone());
    component.descriptor = Some(document);
    Ok(component)
}

fn validate_metadata(metadata: &Value) -> Result<(), ApiError> {
    let invalid = (StatusCode::BAD_REQUEST, "Invalid descriptor metadata");
    for field in ["annotations", "labels"] {
        if let Some(value) = metadata.get(field)
            && !value
                .as_object()
                .is_some_and(|map| map.values().all(Value::is_string))
        {
            return Err(invalid);
        }
    }
    if let Some(tags) = metadata.get("tags")
        && !tags
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    {
        return Err(invalid);
    }
    if let Some(links) = metadata.get("links")
        && !links
            .as_array()
            .is_some_and(|items| items.iter().all(valid_link))
    {
        return Err(invalid);
    }
    for field in ["title", "namespace"] {
        if let Some(value) = metadata.get(field)
            && !value.is_string()
        {
            return Err(invalid);
        }
    }
    Ok(())
}

fn validate_spec(spec: &Value) -> Result<(), ApiError> {
    let invalid = (StatusCode::BAD_REQUEST, "Invalid descriptor specification");
    for field in ["system", "subcomponentOf"] {
        if let Some(value) = spec.get(field)
            && value.as_str().is_none_or(|value| value.trim().is_empty())
        {
            return Err(invalid);
        }
    }
    for field in ["providesApis", "consumesApis", "dependsOn", "dependencyOf"] {
        if let Some(value) = spec.get(field)
            && !value.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|value| !value.trim().is_empty()))
            })
        {
            return Err(invalid);
        }
    }
    Ok(())
}

fn valid_link(link: &Value) -> bool {
    link.get("url").and_then(Value::as_str).is_some_and(|url| {
        url.strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with(['/', '?', '#']))
            && !url.contains(char::is_whitespace)
    }) && ["title", "icon", "type"]
        .iter()
        .all(|field| link.get(field).is_none_or(Value::is_string))
}

fn reject_substitutions(value: &Value, depth: usize) -> Result<(), ApiError> {
    let invalid = (
        StatusCode::BAD_REQUEST,
        "Unsupported descriptor nesting or substitution",
    );
    if depth > 32 {
        return Err(invalid);
    }
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(key.as_str(), "$text" | "$json" | "$yaml") {
                    return Err(invalid);
                }
                reject_substitutions(value, depth + 1)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                reject_substitutions(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_repository::SqliteCatalogRepository;
    use serde_json::json;
    use sqlx::SqlitePool;
    use std::sync::Arc;

    fn state(pool: SqlitePool) -> CatalogState {
        Arc::new(SqliteCatalogRepository::new(pool))
    }

    fn descriptor() -> Value {
        json!({
            "apiVersion": "backstage.io/v1alpha1",
            "kind": "Component",
            "metadata": {
                "name": "billing", "description": "Billing service",
                "annotations": {"example.com/repository": "billing"},
                "labels": {"tier": "backend"}, "tags": ["rust"],
                "links": [{"url": "https://example.com/billing", "title": "Billing"}],
                "custom": {"priority": 2}
            },
            "spec": {"type": "service", "lifecycle": "production", "owner": "team-a", "dependsOn": ["resource:database"]}
        })
    }

    #[tokio::test]
    async fn descriptor_survives_storage_and_legacy_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        let pool = crate::database::open(&path).await.unwrap();
        let document = descriptor();
        let (status, Json(created)) =
            import_document(state(pool.clone()).as_ref(), document.clone())
                .await
                .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created.descriptor.unwrap(), document);
        assert_eq!(created.component_type.as_deref(), Some("service"));
        assert_eq!(created.lifecycle.as_deref(), Some("production"));
        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let Json(reopened) = get_one(State(state(pool.clone())), Path("billing".into()))
            .await
            .unwrap();
        assert_eq!(reopened.descriptor.unwrap(), document);
        let Json(updated) = update(
            State(state(pool)),
            Path("billing".into()),
            Json(NewComponent {
                name: "billing".into(),
                description: "Updated".into(),
                owner: "team-b".into(),
            }),
        )
        .await
        .unwrap();
        let mut expected = document;
        expected["metadata"]["description"] = json!("Updated");
        expected["spec"]["owner"] = json!("team-b");
        assert_eq!(updated.descriptor.unwrap(), expected);
    }

    #[tokio::test]
    async fn additive_migration_preserves_legacy_components() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0001_catalog.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO components (name, description, owner) VALUES ('legacy', 'old', 'team')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0002_catalog_metadata.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let original = r#"{ "metadata": {"name":"billing"}, "spec": {"type":"service","owner":"team","lifecycle":"production"} }"#;
        sqlx::query("INSERT INTO components (name, description, owner, component_type, lifecycle, descriptor) VALUES ('billing', 'billing service', 'team', 'service', 'production', ?)")
            .bind(original).execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0003_catalog_namespaces.sql"))
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let raw: String = sqlx::query_scalar(
            "SELECT descriptor FROM components WHERE namespace = 'default' AND name = 'billing'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(raw, original);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM components")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        let Json(legacy) = get_one(State(state(pool)), Path("legacy".into()))
            .await
            .unwrap();
        assert_eq!(
            (&*legacy.description, &*legacy.owner, &*legacy.namespace),
            ("old", "team", "default")
        );
        assert!(
            legacy.component_type.is_none()
                && legacy.lifecycle.is_none()
                && legacy.descriptor.is_none()
        );
    }

    #[tokio::test]
    async fn namespace_identity_isolation_and_pagination() {
        let dir = tempfile::tempdir().unwrap();
        let repository = state(
            crate::database::open(&dir.path().join("catalog.db"))
                .await
                .unwrap(),
        );
        let default_document = descriptor();
        let (status, Json(_)) = import_document(repository.as_ref(), default_document.clone())
            .await
            .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        let mut other_document = descriptor();
        other_document["metadata"]["namespace"] = json!("platform.dev");
        other_document["spec"]["owner"] = json!("team-other");
        let (status, Json(_)) = import_document(repository.as_ref(), other_document.clone())
            .await
            .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            import_document(repository.as_ref(), other_document.clone())
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        let yaml = serde_yaml_ng::to_string(&other_document).unwrap();
        assert_eq!(
            import_yaml(State(repository.clone()), yaml)
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        let Json(default) = get_one(State(repository.clone()), Path("billing".into()))
            .await
            .unwrap();
        assert_eq!(default.descriptor.unwrap(), default_document);
        let Json(other) = get_qualified(
            State(repository.clone()),
            Path(("platform.dev".into(), "billing".into())),
        )
        .await
        .unwrap();
        assert_eq!(other.descriptor.unwrap(), other_document);
        let Json(updated) = update_qualified(
            State(repository.clone()),
            Path(("platform.dev".into(), "billing".into())),
            Json(NewComponent {
                name: "billing".into(),
                description: "changed".into(),
                owner: "new-owner".into(),
            }),
        )
        .await
        .unwrap();
        other_document["metadata"]["description"] = json!("changed");
        other_document["spec"]["owner"] = json!("new-owner");
        assert_eq!(updated.descriptor.unwrap(), other_document);
        let Json(default) = get_one(State(repository.clone()), Path("billing".into()))
            .await
            .unwrap();
        assert_eq!(default.descriptor.unwrap(), default_document);
        for (offset, expected) in [(0, "default"), (1, "platform.dev")] {
            let Json(page) = list(
                State(repository.clone()),
                Query(Filter {
                    q: "billing".into(),
                    offset,
                    namespace: None,
                    limit: 1,
                }),
            )
            .await
            .unwrap();
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].namespace, expected);
        }
        let Json(page) = list(
            State(repository.clone()),
            Query(Filter {
                q: "".into(),
                offset: 0,
                namespace: Some("platform.dev".into()),
                limit: 500,
            }),
        )
        .await
        .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].namespace, "platform.dev");
        for limit in [0, 501] {
            assert_eq!(
                list(
                    State(repository.clone()),
                    Query(Filter {
                        q: "".into(),
                        offset: 0,
                        namespace: None,
                        limit
                    })
                )
                .await
                .unwrap_err()
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            remove(State(repository.clone()), Path("billing".into()))
                .await
                .unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            get_one(State(repository.clone()), Path("billing".into()))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            remove(State(repository.clone()), Path("billing".into()))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            remove_qualified(
                State(repository.clone()),
                Path(("platform.dev".into(), "billing".into()))
            )
            .await
            .unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            get_qualified(
                State(repository),
                Path(("platform.dev".into(), "billing".into()))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn yaml_import_uses_same_validation_and_preserves_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("catalog.db"))
            .await
            .unwrap();
        let yaml = "apiVersion: backstage.io/v1alpha1\nkind: Component\nmetadata:\n  name: billing\n  namespace: platform\n  tags: [rust]\nspec:\n  type: service\n  lifecycle: production\n  owner: team-a\n";
        let (_, Json(created)) = import_yaml(State(state(pool.clone())), yaml.into())
            .await
            .unwrap();
        assert_eq!(created.namespace, "platform");
        assert_eq!(
            created.descriptor.unwrap()["metadata"]["tags"],
            json!(["rust"])
        );
        assert_eq!(
            import_yaml(State(state(pool.clone())), yaml.into())
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            import_yaml(State(state(pool.clone())), format!("{yaml}---\n{yaml}"))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            import_yaml(State(state(pool)), "metadata: [".into())
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn descriptor_validation_rejects_unsupported_and_invalid_fields() {
        for (field, value) in [
            ("namespace", json!("Other")),
            ("namespace", json!("../other")),
            ("namespace", json!("")),
            ("namespace", Value::Null),
            ("annotations", json!({"bad": 42})),
            ("tags", json!("rust")),
            ("links", json!([{"url": "javascript:alert(1)"}])),
            ("links", json!([{"url": "https://"}])),
        ] {
            let mut document = descriptor();
            document["metadata"][field] = value;
            assert_eq!(
                validate_descriptor(document).unwrap_err().0,
                StatusCode::BAD_REQUEST
            );
        }
        let mut document = descriptor();
        document["spec"]["owner"] = json!({"$text": "https://example.com/team"});
        assert_eq!(
            validate_descriptor(document).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        let mut document = descriptor();
        document["metadata"]["custom"] = json!("x".repeat(MAX_DESCRIPTOR_BYTES));
        assert_eq!(
            validate_descriptor(document).unwrap_err().0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
