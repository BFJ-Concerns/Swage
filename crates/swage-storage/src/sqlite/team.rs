use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewTeam, Team, TeamAgentDefinition, TeamTaskDefinition};
use crate::sqlite::SqliteStorage;
use crate::store::TeamStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_team(row: &rusqlite::Row<'_>) -> rusqlite::Result<Team> {
    let id: String = row.get(0)?;
    let project_id: String = row.get(1)?;
    let members_json: String = row.get(5)?;
    let agents_json: String = row.get(6)?;
    let tasks_json: String = row.get(7)?;
    let created: String = row.get(8)?;
    let updated: String = row.get(9)?;

    let agents = parse_agents(&agents_json)?;

    Ok(Team {
        id: parse_uuid(&id)?,
        project_id: parse_uuid(&project_id)?,
        name: row.get(2)?,
        description: row.get(3)?,
        entry_agent_id: row.get(4)?,
        members: parse_members(&members_json)?,
        agents,
        tasks: parse_tasks(&tasks_json)?,
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

fn parse_agents(s: &str) -> rusqlite::Result<Vec<TeamAgentDefinition>> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_tasks(s: &str) -> rusqlite::Result<Vec<TeamTaskDefinition>> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(crate) fn migrate_team_authoring_model(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "
        BEGIN;
        ALTER TABLE teams ADD COLUMN agents_json TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE teams ADD COLUMN tasks_json TEXT NOT NULL DEFAULT '[]';
        COMMIT;
        ",
    )
    .map_err(|error| {
        StorageError::MigrationFailed(format!("failed to add team authoring columns: {error}"))
    })?;

    let mut stmt = conn
        .prepare("SELECT id, entry_agent_id, members FROM teams")
        .map_err(|error| {
            StorageError::MigrationFailed(format!(
                "failed to prepare team authoring backfill query: {error}"
            ))
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| {
            StorageError::MigrationFailed(format!(
                "failed to enumerate team authoring backfill rows: {error}"
            ))
        })?;

    for row in rows {
        let (id, entry_agent_id, members_json) = row.map_err(|error| {
            StorageError::MigrationFailed(format!(
                "failed to read team authoring backfill row: {error}"
            ))
        })?;
        let mut member_ids = parse_members(&members_json).map_err(|error| {
            StorageError::MigrationFailed(format!(
                "failed to decode legacy team members for {id}: {error}"
            ))
        })?;
        if !member_ids.iter().any(|member| member == &entry_agent_id) {
            member_ids.insert(0, entry_agent_id.clone());
        }
        let agents = member_ids
            .into_iter()
            .map(|agent_id| TeamAgentDefinition {
                display_name: agent_id.clone(),
                id: agent_id,
                description: String::new(),
                instructions: String::new(),
                model: None,
            })
            .collect::<Vec<_>>();
        let agents_json = serde_json::to_string(&agents).map_err(|error| {
            StorageError::MigrationFailed(format!(
                "failed to serialise team agents for {id}: {error}"
            ))
        })?;
        conn.execute(
            "UPDATE teams SET agents_json = ?2, tasks_json = '[]' WHERE id = ?1",
            params![id, agents_json],
        )
        .map_err(|error| {
            StorageError::MigrationFailed(format!(
                "failed to write team authoring backfill for {id}: {error}"
            ))
        })?;
    }

    Ok(())
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
            let members = Team::member_ids_from_agents(&team.agents);
            let members_json = serde_json::to_string(&members).map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?;
            let agents_json = serde_json::to_string(&team.agents).map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?;
            let tasks_json = serde_json::to_string(&team.tasks).map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?;

            conn.execute(
                "INSERT INTO teams (id, project_id, name, description, entry_agent_id, members, agents_json, tasks_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    id.to_string(),
                    team.project_id.to_string(),
                    &team.name,
                    &team.description,
                    &team.entry_agent_id,
                    members_json,
                    agents_json,
                    tasks_json,
                    &now_str,
                ],
            )?;

            Ok(Team {
                id,
                project_id: team.project_id,
                name: team.name,
                description: team.description,
                entry_agent_id: team.entry_agent_id,
                members,
                agents: team.agents,
                tasks: team.tasks,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_team(&self, id: Uuid) -> Result<Option<Team>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, project_id, name, description, entry_agent_id, members, agents_json, tasks_json, created_at, updated_at
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
                "SELECT id, project_id, name, description, entry_agent_id, members, agents_json, tasks_json, created_at, updated_at
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
                "SELECT id, project_id, name, description, entry_agent_id, members, agents_json, tasks_json, created_at, updated_at
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
        agents: Option<Vec<TeamAgentDefinition>>,
        tasks: Option<Vec<TeamTaskDefinition>>,
    ) -> Result<Team, StorageError> {
        let _existing = self
            .get_team(id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "Team".into(),
                id: id.to_string(),
            })?;

        let agents_json =
            match agents.as_ref() {
                Some(agents) => Some(serde_json::to_string(agents).map_err(|e| {
                    StorageError::Serialisation {
                        message: e.to_string(),
                    }
                })?),
                None => None,
            };
        let tasks_json = match tasks.as_ref() {
            Some(tasks) => {
                Some(
                    serde_json::to_string(tasks).map_err(|e| StorageError::Serialisation {
                        message: e.to_string(),
                    })?,
                )
            }
            None => None,
        };
        let members_json = match agents.as_ref() {
            Some(agents) => Some(
                serde_json::to_string(&Team::member_ids_from_agents(agents)).map_err(|e| {
                    StorageError::Serialisation {
                        message: e.to_string(),
                    }
                })?,
            ),
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
                     agents_json = COALESCE(?6, agents_json),
                     tasks_json = COALESCE(?7, tasks_json),
                     updated_at = ?8
                 WHERE id = ?1",
                params![
                    id_str,
                    name,
                    description,
                    entry_agent_id,
                    members_json,
                    agents_json,
                    tasks_json,
                    now
                ],
            )?;

            conn.query_row(
                "SELECT id, project_id, name, description, entry_agent_id, members, agents_json, tasks_json, created_at, updated_at
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

    fn agent(id: &str) -> crate::model::TeamAgentDefinition {
        crate::model::TeamAgentDefinition {
            id: id.to_string(),
            display_name: id.to_string(),
            description: String::new(),
            instructions: String::new(),
            model: None,
        }
    }

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
                agents: vec![agent("planner"), agent("reviewer")],
                tasks: Vec::new(),
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
                agents: vec![agent("planner")],
                tasks: Vec::new(),
            })
            .await
            .unwrap();
        storage
            .create_team(NewTeam {
                project_id: b.id,
                name: "shared-name".into(),
                description: String::new(),
                entry_agent_id: "reviewer".into(),
                agents: vec![agent("reviewer")],
                tasks: Vec::new(),
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
                agents: vec![agent("planner")],
                tasks: Vec::new(),
            })
            .await
            .unwrap();

        let updated = storage
            .update_team(
                created.id,
                Some("review-team".into()),
                Some("Updated".into()),
                Some("reviewer".into()),
                Some(vec![agent("reviewer"), agent("critic")]),
                None,
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
                agents: vec![agent("a")],
                tasks: Vec::new(),
            })
            .await
            .unwrap();
        storage
            .create_team(NewTeam {
                project_id: project.id,
                name: "second".into(),
                description: String::new(),
                entry_agent_id: "b".into(),
                agents: vec![agent("b")],
                tasks: Vec::new(),
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
                agents: vec![agent("agent")],
                tasks: Vec::new(),
            })
            .await
            .unwrap();

        assert!(storage.delete_team(created.id).await.unwrap());
        assert!(!storage.delete_team(created.id).await.unwrap());
    }
}
