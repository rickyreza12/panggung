use crate::catalog::Component;
use crate::catalog_relations::{Relation, ResolvedRelation, reference_parts};
use serde_json::Value;
use sqlx::{SqlitePool, types::Json};
use std::{future::Future, pin::Pin, sync::Arc};

#[derive(Debug, PartialEq, Eq)]
pub enum RepositoryError {
    Conflict,
    Unavailable,
}

type RepositoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RepositoryError>> + Send + 'a>>;
pub type CatalogState = Arc<dyn CatalogRepository>;

pub trait CatalogRepository: Send + Sync {
    fn list<'a>(
        &'a self,
        query: &'a str,
        namespace: Option<&'a str>,
        offset: u32,
        limit: u32,
    ) -> RepositoryFuture<'a, Vec<Component>>;
    fn insert(&self, component: Component) -> RepositoryFuture<'_, Component>;
    fn get<'a>(
        &'a self,
        namespace: &'a str,
        name: &'a str,
    ) -> RepositoryFuture<'a, Option<Component>>;
    // Updates the descriptor and indexed fields atomically.
    fn update<'a>(
        &'a self,
        namespace: &'a str,
        name: &'a str,
        description: &'a str,
        owner: &'a str,
    ) -> RepositoryFuture<'a, Option<Component>>;
    fn remove<'a>(&'a self, namespace: &'a str, name: &'a str) -> RepositoryFuture<'a, bool>;
    fn ready(&self) -> RepositoryFuture<'_, ()>;
    fn resolve_relations(
        &self,
        relations: Vec<Relation>,
    ) -> RepositoryFuture<'_, Vec<ResolvedRelation>>;
}

pub struct SqliteCatalogRepository {
    pool: SqlitePool,
}

impl SqliteCatalogRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct ComponentRow {
    name: String,
    description: String,
    owner: String,
    namespace: String,
    component_type: Option<String>,
    lifecycle: Option<String>,
    descriptor: Option<Json<Value>>,
}

impl From<ComponentRow> for Component {
    fn from(row: ComponentRow) -> Self {
        Self {
            name: row.name,
            description: row.description,
            owner: row.owner,
            namespace: row.namespace,
            component_type: row.component_type,
            lifecycle: row.lifecycle,
            descriptor: row.descriptor.map(|value| value.0),
        }
    }
}

fn repository_error(error: sqlx::Error) -> RepositoryError {
    if let sqlx::Error::Database(ref db) = error
        && db.is_unique_violation()
    {
        return RepositoryError::Conflict;
    }
    RepositoryError::Unavailable
}

impl CatalogRepository for SqliteCatalogRepository {
    fn resolve_relations(
        &self,
        relations: Vec<Relation>,
    ) -> RepositoryFuture<'_, Vec<ResolvedRelation>> {
        Box::pin(async move {
            let mut transaction = self.pool.begin().await.map_err(repository_error)?;
            let mut result = Vec::with_capacity(relations.len());
            let mut cache = std::collections::HashMap::new();
            for relation in relations {
                let resolved = if let Some(resolved) = cache.get(&relation.target_ref) {
                    *resolved
                } else {
                    let (kind, namespace, name) = reference_parts(&relation.target_ref)
                        .ok_or(RepositoryError::Unavailable)?;
                    let exists: bool = match kind {
                        "component" => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM components WHERE namespace = ? AND name = ?)")
                            .bind(namespace).bind(name).fetch_one(&mut *transaction).await.map_err(repository_error)?,
                        "system" => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catalog_systems WHERE namespace = ? AND name = ?)")
                            .bind(namespace).bind(name).fetch_one(&mut *transaction).await.map_err(repository_error)?,
                        "group" if namespace == "default" => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM identity_teams WHERE name = ?)")
                            .bind(name).fetch_one(&mut *transaction).await.map_err(repository_error)?,
                        "user" if namespace == "default" => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM identity_users WHERE username = ?)")
                            .bind(name).fetch_one(&mut *transaction).await.map_err(repository_error)?,
                        _ => false,
                    };
                    cache.insert(relation.target_ref.clone(), exists);
                    exists
                };
                result.push(ResolvedRelation { relation, resolved });
            }
            transaction.commit().await.map_err(repository_error)?;
            Ok(result)
        })
    }

    fn list<'a>(
        &'a self,
        query: &'a str,
        namespace: Option<&'a str>,
        offset: u32,
        limit: u32,
    ) -> RepositoryFuture<'a, Vec<Component>> {
        Box::pin(async move {
            sqlx::query_as::<_, ComponentRow>("SELECT name, description, owner, namespace, component_type, lifecycle, descriptor FROM components WHERE (? IS NULL OR namespace = ?) AND (instr(lower(name || ' ' || description || ' ' || owner), lower(?)) > 0 OR ? = '') ORDER BY name, namespace LIMIT ? OFFSET ?")
                .bind(namespace).bind(namespace).bind(query).bind(query).bind(limit).bind(offset)
                .fetch_all(&self.pool).await
                .map(|rows| rows.into_iter().map(Component::from).collect())
                .map_err(repository_error)
        })
    }

    fn insert(&self, component: Component) -> RepositoryFuture<'_, Component> {
        Box::pin(async move {
            sqlx::query("INSERT INTO components (name, description, owner, namespace, component_type, lifecycle, descriptor) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(&component.name)
                .bind(&component.description)
                .bind(&component.owner)
                .bind(&component.namespace)
                .bind(&component.component_type)
                .bind(&component.lifecycle)
                .bind(component.descriptor.as_ref().map(Json))
                .execute(&self.pool).await.map_err(repository_error)?;
            Ok(component)
        })
    }

    fn get<'a>(
        &'a self,
        namespace: &'a str,
        name: &'a str,
    ) -> RepositoryFuture<'a, Option<Component>> {
        Box::pin(async move {
            sqlx::query_as::<_, ComponentRow>("SELECT name, description, owner, namespace, component_type, lifecycle, descriptor FROM components WHERE namespace = ? AND name = ?")
                .bind(namespace).bind(name).fetch_optional(&self.pool).await
                .map(|row| row.map(Component::from))
                .map_err(repository_error)
        })
    }

    fn update<'a>(
        &'a self,
        namespace: &'a str,
        name: &'a str,
        description: &'a str,
        owner: &'a str,
    ) -> RepositoryFuture<'a, Option<Component>> {
        Box::pin(async move {
            sqlx::query_as::<_, ComponentRow>("UPDATE components SET description = ?, owner = ?, descriptor = CASE WHEN descriptor IS NULL THEN NULL ELSE json_set(descriptor, '$.metadata.description', ?, '$.spec.owner', ?) END WHERE namespace = ? AND name = ? RETURNING name, description, owner, namespace, component_type, lifecycle, descriptor")
                .bind(description).bind(owner).bind(description).bind(owner).bind(namespace).bind(name)
                .fetch_optional(&self.pool).await
                .map(|row| row.map(Component::from))
                .map_err(repository_error)
        })
    }

    fn remove<'a>(&'a self, namespace: &'a str, name: &'a str) -> RepositoryFuture<'a, bool> {
        Box::pin(async move {
            sqlx::query("DELETE FROM components WHERE namespace = ? AND name = ?")
                .bind(namespace)
                .bind(name)
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected() != 0)
                .map_err(repository_error)
        })
    }

    fn ready(&self) -> RepositoryFuture<'_, ()> {
        Box::pin(async move {
            sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&self.pool)
                .await
                .map_err(repository_error)
                .and_then(|value| {
                    if value == 1 {
                        Ok(())
                    } else {
                        Err(RepositoryError::Unavailable)
                    }
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relationship_resolution_tracks_actual_targets_and_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("relations.db"))
            .await
            .unwrap();
        let repo = SqliteCatalogRepository::new(pool.clone());
        let spec = serde_json::json!({"owner":"group:default/platform", "system":"billing", "dependsOn":["backend", "component:other/backend", "resource:cache"], "providesApis":["public"]});
        let projected = || crate::catalog_relations::project(&spec, "platform").unwrap();
        assert!(
            repo.resolve_relations(projected())
                .await
                .unwrap()
                .iter()
                .all(|relation| !relation.resolved)
        );
        sqlx::query("INSERT INTO identity_teams(name) VALUES ('platform')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO catalog_systems(namespace,name) VALUES ('platform','billing')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO components(namespace,name,description,owner) VALUES ('platform','backend','','platform')").execute(&pool).await.unwrap();
        let result = repo.resolve_relations(projected()).await.unwrap();
        assert_eq!(
            result
                .iter()
                .map(|relation| relation.resolved)
                .collect::<Vec<_>>(),
            vec![true, true, true, false, false, false]
        );
        let identity = crate::identity::IdentityStore::new(pool.clone());
        use rand_core::{OsRng, RngCore};
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let password: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        identity.bootstrap_admin("alice", &password).await.unwrap();
        let user_refs = vec![
            Relation {
                relation_type: "ownedBy",
                target_ref: "user:default/alice".into(),
            },
            Relation {
                relation_type: "ownedBy",
                target_ref: "user:other/alice".into(),
            },
        ];
        let result = repo.resolve_relations(user_refs).await.unwrap();
        assert!(result[0].resolved);
        assert!(!result[1].resolved);
        repo.remove("platform", "backend").await.unwrap();
        assert!(!repo.resolve_relations(projected()).await.unwrap()[2].resolved);
        pool.close().await;
        assert_eq!(
            repo.resolve_relations(projected()).await.unwrap_err(),
            RepositoryError::Unavailable
        );
    }

    #[tokio::test]
    async fn closed_pool_reports_failure_instead_of_empty_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("catalog.db"))
            .await
            .unwrap();
        let repo: CatalogState = Arc::new(SqliteCatalogRepository::new(pool.clone()));
        assert!(repo.list("", None, 0, 500).await.unwrap().is_empty());
        pool.close().await;
        assert_eq!(
            repo.list("", None, 0, 500).await.unwrap_err(),
            RepositoryError::Unavailable
        );
        assert_eq!(
            repo.get("default", "missing").await.unwrap_err(),
            RepositoryError::Unavailable
        );
        assert_eq!(
            repo.ready().await.unwrap_err(),
            RepositoryError::Unavailable
        );
    }
}
