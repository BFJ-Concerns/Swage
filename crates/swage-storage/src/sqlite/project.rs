use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewProject, Project};
use crate::sqlite::SqliteStorage;
use crate::store::ProjectStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let id: String = row.get(0)?;
    // Column order: id(0), name(1), slug(2), workspace_path(3), created_at(4), updated_at(5)
    let created: String = row.get(4)?;
    let updated: String = row.get(5)?;

    Ok(Project {
        id: parse_uuid(&id)?,
        name: row.get(1)?,
        slug: row.get(2)?,
        workspace_path: row.get(3)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

// ---------------------------------------------------------------------------
// ProjectStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ProjectStore for SqliteStorage {
    async fn create(&self, project: NewProject) -> Result<Project, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO projects (id, name, slug, workspace_path, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id.to_string(), &project.name, &project.slug, &project.workspace_path, &now_str],
            )?;

            Ok(Project {
                id,
                name: project.name,
                slug: project.slug,
                workspace_path: project.workspace_path,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get(&self, id: Uuid) -> Result<Option<Project>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, name, slug, workspace_path, created_at, updated_at
                 FROM projects WHERE id = ?1",
                params![id.to_string()],
                row_to_project,
            )
            .optional()
        })
        .await
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Project>, StorageError> {
        // Capture as owned String to satisfy 'static bound on with_conn closure.
        let name = name.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, name, slug, workspace_path, created_at, updated_at
                 FROM projects WHERE name = ?1",
                params![&name],
                row_to_project,
            )
            .optional()
        })
        .await
    }

    async fn get_or_create_default(&self) -> Result<Project, StorageError> {
        // The default project is seeded by migrations, but get_by_name is used
        // here in case the caller invokes this on a storage not yet seeded.
        if let Some(project) = self.get_by_name("default").await? {
            return Ok(project);
        }

        self.create(NewProject { name: "default".into(), slug: "default".into(), workspace_path: None })
            .await
    }

    async fn list_all(&self) -> Result<Vec<Project>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, slug, workspace_path, created_at, updated_at
                 FROM projects ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], row_to_project)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<Project>, StorageError> {
        let slug = slug.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, name, slug, workspace_path, created_at, updated_at
                 FROM projects WHERE slug = ?1",
                params![&slug],
                row_to_project,
            )
            .optional()
        })
        .await
    }

    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        slug: Option<String>,
        workspace_path: Option<String>,
    ) -> Result<Project, StorageError> {
        // Pre-flight: verify the project exists.
        if self.get(id).await?.is_none() {
            return Err(StorageError::NotFound {
                entity: "Project".into(),
                id: id.to_string(),
            });
        }

        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            let id_str = id.to_string();

            // COALESCE preserves the existing value when the parameter is NULL,
            // so None fields stay unchanged while Some fields are overwritten.
            conn.execute(
                "UPDATE projects
                 SET name           = COALESCE(?2, name),
                     slug           = COALESCE(?3, slug),
                     workspace_path = COALESCE(?4, workspace_path),
                     updated_at     = ?5
                 WHERE id = ?1",
                params![id_str, name, slug, workspace_path, now],
            )?;

            conn.query_row(
                "SELECT id, name, slug, workspace_path, created_at, updated_at
                 FROM projects WHERE id = ?1",
                params![id_str],
                row_to_project,
            )
        })
        .await
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        // Pre-flight: fetch the project to check existence and guard the default.
        match self.get(id).await? {
            None => return Ok(false),
            Some(ref p) if p.name == "default" => {
                return Err(StorageError::ConstraintViolation(
                    "cannot delete the default project".into(),
                ))
            }
            Some(_) => {}
        }

        // Sessions cascade via the FK ON DELETE CASCADE defined in the schema.
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM projects WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStorage;

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_project() {
        let storage = SqliteStorage::in_memory().unwrap();
        let new = NewProject { name: "my-project".into(), slug: "my-project".into(), workspace_path: Some("/tmp".into()) };
        let created = storage.create(new).await.unwrap();

        assert_eq!(created.name, "my-project");
        assert_eq!(created.slug, "my-project");
        assert_eq!(created.workspace_path, Some("/tmp".into()));

        let fetched = storage.get(created.id).await.unwrap().expect("should exist");
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.name, "my-project");
        assert_eq!(fetched.slug, "my-project");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_name_returns_none_for_unknown() {
        let storage = SqliteStorage::in_memory().unwrap();
        let result = storage.get_by_name("no-such-project").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_name_finds_existing() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .create(NewProject { name: "find-me".into(), slug: "find-me".into(), workspace_path: None })
            .await
            .unwrap();

        let found = storage.get_by_name("find-me").await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_or_create_default_is_idempotent() {
        let storage = SqliteStorage::in_memory().unwrap();
        let p1 = storage.get_or_create_default().await.unwrap();
        let p2 = storage.get_or_create_default().await.unwrap();
        // Same project returned both times — not duplicated.
        assert_eq!(p1.id, p2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slug_is_stored_and_retrieved() {
        let storage = SqliteStorage::in_memory().unwrap();
        let new = NewProject {
            name: "slugged-project".into(),
            slug: "slugged-project".into(),
            workspace_path: None,
        };
        let created = storage.create(new).await.unwrap();
        assert_eq!(created.slug, "slugged-project");

        let fetched = storage.get(created.id).await.unwrap().expect("should exist");
        assert_eq!(fetched.slug, "slugged-project");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_or_create_default_returns_default_slug() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();
        assert_eq!(project.slug, "default");
    }

    // ---------------------------------------------------------------------------
    // list_all
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn list_all_returns_projects_ordered_by_created_at() {
        let storage = SqliteStorage::in_memory().unwrap();
        // The seeded default project already exists; add two more.
        storage.create(NewProject { name: "alpha".into(), slug: "alpha".into(), workspace_path: None }).await.unwrap();
        storage.create(NewProject { name: "beta".into(),  slug: "beta".into(),  workspace_path: None }).await.unwrap();

        let all = storage.list_all().await.unwrap();

        // At least 3 projects (default + alpha + beta).
        assert!(all.len() >= 3, "expected at least 3 projects, got {}", all.len());

        // Must be ordered by created_at ASC.
        let is_sorted = all.windows(2).all(|w| w[0].created_at <= w[1].created_at);
        assert!(is_sorted, "projects should be ordered by created_at ASC");
    }

    // ---------------------------------------------------------------------------
    // get_by_slug
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_slug_returns_matching_project() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage.create(NewProject { name: "Slug Test".into(), slug: "slug-test".into(), workspace_path: None }).await.unwrap();

        let found = storage.get_by_slug("slug-test").await.unwrap();
        assert!(found.is_some(), "get_by_slug should find the project");
        assert_eq!(found.unwrap().slug, "slug-test");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_slug_returns_none_for_missing() {
        let storage = SqliteStorage::in_memory().unwrap();
        let result = storage.get_by_slug("no-such-slug").await.unwrap();
        assert!(result.is_none());
    }

    // ---------------------------------------------------------------------------
    // update
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn update_changes_provided_fields() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage.create(NewProject { name: "Old Name".into(), slug: "old-name".into(), workspace_path: None }).await.unwrap();

        let updated = storage.update(
            created.id,
            Some("New Name".into()),
            Some("new-name".into()),
            None,
        ).await.unwrap();

        assert_eq!(updated.name, "New Name");
        assert_eq!(updated.slug, "new-name");
        // workspace_path was None before and should remain None.
        assert!(updated.workspace_path.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_partial_leaves_unspecified_fields_unchanged() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage.create(NewProject { name: "Stable".into(), slug: "stable".into(), workspace_path: Some("/opt".into()) }).await.unwrap();

        // Only update workspace_path — name and slug stay.
        let updated = storage.update(created.id, None, None, Some("/new".into())).await.unwrap();

        assert_eq!(updated.name, "Stable");
        assert_eq!(updated.slug, "stable");
        assert_eq!(updated.workspace_path, Some("/new".into()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_errors_for_unknown_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let nonexistent = Uuid::new_v4();
        let result = storage.update(nonexistent, Some("X".into()), None, None).await;
        assert!(result.is_err(), "update on unknown id should return an error");
    }

    // ---------------------------------------------------------------------------
    // delete
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_removes_project_and_returns_true() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage.create(NewProject { name: "Gone".into(), slug: "gone".into(), workspace_path: None }).await.unwrap();

        let deleted = storage.delete(created.id).await.unwrap();
        assert!(deleted, "delete should return true for an existing project");

        let fetched = storage.get(created.id).await.unwrap();
        assert!(fetched.is_none(), "project should no longer exist after delete");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_returns_false_for_unknown_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let nonexistent = Uuid::new_v4();
        let result = storage.delete(nonexistent).await.unwrap();
        assert!(!result, "delete on unknown id should return false");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_refuses_default_project() {
        let storage = SqliteStorage::in_memory().unwrap();
        let default_project = storage.get_or_create_default().await.unwrap();

        let result = storage.delete(default_project.id).await;
        assert!(result.is_err(), "deleting the default project should return an error");
    }
}
