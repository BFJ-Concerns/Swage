use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewTeam, Team};
use crate::sqlite::SqliteStorage;
use crate::store::TeamStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_team(row: &rusqlite::Row<'_>) -> rusqlite::Result<Team> {
    let id: String = row.get(0)?;
    let project_id: String = row.get(1)?;
    let members_json: String = row.get(5)?;
    let created: String = row.get(6)?;
    let updated: String = row.get(7)?;

    Ok(Team {
        id: parse_uuid(&id)?,
        project_id: parse_uuid(&project_id)?,
        name: row.get(2)?,
        description: row.get(3)?,
        entry_agent_id: row.get(4)?,
        members: parse_members(&members_json)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn parse_members(s: &str) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

// ---------------------------------------------------------------------------
// TeamStore
// ---------------------------------------------------------------------------

#[async_trait]
impl TeamStore for SqliteStorage {
    async fn create_team(&self, team: NewTeam) -> Result<Team, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let members_json = serde_json::to_string(&team.members).map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?;

            conn.execute(
                "INSERT INTO teams (id, project_id, name, description, entry_agent_id, members, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id.to_string(),
                    team.project_id.to_string(),
                    &team.name,
                    &team.description,
                    &team.entry_agent_id,
                    members_json,
                    &now_str,
                ],
            )?;

            Ok(Team {
                id,
                project_id: team.project_id,
                name: team.name,
                description: team.description,
                entry_agent_id: team.entry_agent_id,
                members: team.members,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_team(&self, id: Uuid) -> Result<Option<Team>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, project_id, name, description, entry_agent_id, members, created_at, updated_at
                 FROM teams WHERE id = ?1",
                params![id.to_string()],
                row_to_team,
            )
            .optional()
        })
        .await
    }

    async fn get_team_by_name(
        &self,
        project_id: Uuid,
        name: &str,
    ) -> Result<Option<Team>, StorageError> {
        let name = name.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, project_id, name, description, entry_agent_id, members, created_at, updated_at
                 FROM teams WHERE project_id = ?1 AND name = ?2",
                params![project_id.to_string(), name],
                row_to_team,
            )
            .optional()
        })
        .await
    }

    async fn list_teams_by_project(&self, project_id: Uuid) -> Result<Vec<Team>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project_id, name, description, entry_agent_id, members, created_at, updated_at
                 FROM teams WHERE project_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![project_id.to_string()], row_to_team)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn update_team(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        entry_agent_id: Option<String>,
        members: Option<Vec<String>>,
    ) -> Result<Team, StorageError> {
        if self.get_team(id).await?.is_none() {
            return Err(StorageError::NotFound {
                entity: "Team".into(),
                id: id.to_string(),
            });
        }

        let members_json =
            match members {
                Some(members) => Some(serde_json::to_string(&members).map_err(|e| {
                    StorageError::Serialisation {
                        message: e.to_string(),
                    }
                })?),
                None => None,
            };

        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            let id_str = id.to_string();

            conn.execute(
                "UPDATE teams
                 SET name = COALESCE(?2, name),
                     description = COALESCE(?3, description),
                     entry_agent_id = COALESCE(?4, entry_agent_id),
                     members = COALESCE(?5, members),
                     updated_at = ?6
                 WHERE id = ?1",
                params![id_str, name, description, entry_agent_id, members_json, now],
            )?;

            conn.query_row(
                "SELECT id, project_id, name, description, entry_agent_id, members, created_at, updated_at
                 FROM teams WHERE id = ?1",
                params![id.to_string()],
                row_to_team,
            )
        })
        .await
    }

    async fn delete_team(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(move |conn| {
            let affected =
                conn.execute("DELETE FROM teams WHERE id = ?1", params![id.to_string()])?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStorage;
    use crate::store::ProjectStore;

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_team() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();

        let created = storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "research".into(),
                description: "Research team".into(),
                entry_agent_id: "planner".into(),
                members: vec!["planner".into(), "reviewer".into()],
            })
            .await
            .unwrap();

        let fetched = storage
            .get_team(created.id)
            .await
            .unwrap()
            .expect("team exists");
        assert_eq!(fetched.name, "research");
        assert_eq!(fetched.entry_agent_id, "planner");
        assert_eq!(fetched.members, vec!["planner", "reviewer"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_team_by_name_scopes_to_project() {
        let storage = SqliteStorage::in_memory().unwrap();
        let a = storage
            .create(crate::model::NewProject {
                name: "alpha".into(),
                slug: "alpha".into(),
                workspace_path: None,
            })
            .await
            .unwrap();
        let b = storage
            .create(crate::model::NewProject {
                name: "beta".into(),
                slug: "beta".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        storage
            .create_team(NewTeam {
                project_id: a.id,
                name: "shared-name".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                members: vec!["planner".into()],
            })
            .await
            .unwrap();
        storage
            .create_team(NewTeam {
                project_id: b.id,
                name: "shared-name".into(),
                description: String::new(),
                entry_agent_id: "reviewer".into(),
                members: vec!["reviewer".into()],
            })
            .await
            .unwrap();

        let found = storage
            .get_team_by_name(b.id, "shared-name")
            .await
            .unwrap()
            .expect("team exists");
        assert_eq!(found.project_id, b.id);
        assert_eq!(found.entry_agent_id, "reviewer");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_team_changes_members_and_entry_agent() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();

        let created = storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "review".into(),
                description: "Initial".into(),
                entry_agent_id: "planner".into(),
                members: vec!["planner".into()],
            })
            .await
            .unwrap();

        let updated = storage
            .update_team(
                created.id,
                Some("review-team".into()),
                Some("Updated".into()),
                Some("reviewer".into()),
                Some(vec!["reviewer".into(), "critic".into()]),
            )
            .await
            .unwrap();

        assert_eq!(updated.name, "review-team");
        assert_eq!(updated.description, "Updated");
        assert_eq!(updated.entry_agent_id, "reviewer");
        assert_eq!(updated.members, vec!["reviewer", "critic"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_teams_by_project_orders_by_created_at() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();

        storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "first".into(),
                description: String::new(),
                entry_agent_id: "a".into(),
                members: vec!["a".into()],
            })
            .await
            .unwrap();
        storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "second".into(),
                description: String::new(),
                entry_agent_id: "b".into(),
                members: vec!["b".into()],
            })
            .await
            .unwrap();

        let teams = storage.list_teams_by_project(project.id).await.unwrap();
        assert_eq!(
            teams
                .iter()
                .map(|team| team.name.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_team_returns_true_once() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();

        let created = storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "temporary".into(),
                description: String::new(),
                entry_agent_id: "agent".into(),
                members: vec!["agent".into()],
            })
            .await
            .unwrap();

        assert!(storage.delete_team(created.id).await.unwrap());
        assert!(!storage.delete_team(created.id).await.unwrap());
    }
}
