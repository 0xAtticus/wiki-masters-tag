use crate::api::User;
use anyhow::Result;
use sqlx::prelude::FromRow;
use std::fs::OpenOptions;

pub struct LocalState {
    pub users: UsersState,
}

pub struct UsersState {
    users: sqlx::SqlitePool,
    prefixes: sqlx::SqlitePool,
}

impl LocalState {
    pub async fn new() -> Self {
        Self {
            users: UsersState::new().await,
        }
    }
}

pub enum GetUserResult {
    UserDoesNotExists,
    CacheMiss,
    User(User),
}

#[derive(Debug, FromRow)]
pub struct LocalUser {
    pub id: Option<String>,
    pub name: String,
}

impl UsersState {
    async fn new() -> Self {
        let file_path = "users.db";
        let mut open_option = OpenOptions::new();
        open_option.create_new(true);
        open_option.write(true);
        let _ = open_option.open(file_path);

        let users_db = sqlx::SqlitePool::connect(file_path).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS users(id TEXT, name TEXT PRIMARY KEY)")
            .execute(&users_db)
            .await
            .unwrap();
        sqlx::query("CREATE INDEX IF NOT EXISTS idx ON users(name)")
            .execute(&users_db)
            .await
            .unwrap();

        let file_path = "prefixes.db";
        let _ = open_option.open(file_path);
        let prefixes_db = sqlx::SqlitePool::connect(file_path).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS prefixes(prefix TEXT PRIMARY KEY, count INTEGER)")
            .execute(&prefixes_db)
            .await
            .unwrap();
        sqlx::query("CREATE INDEX IF NOT EXISTS idx ON prefixes(prefix)")
            .execute(&prefixes_db)
            .await
            .unwrap();
        Self {
            users: users_db,
            prefixes: prefixes_db,
        }
    }

    /// Storing with empty id means that user does not exists
    pub async fn store_user(&self, user: LocalUser) -> Result<()> {
        sqlx::query("INSERT INTO users VALUES (?, ?) ON CONFLICT DO NOTHING")
            .bind(user.id)
            .bind(user.name)
            .execute(&self.users)
            .await?;
        Ok(())
    }

    pub async fn get_user_by_name(&self, username: &str) -> Result<GetUserResult> {
        let user: Option<LocalUser> = sqlx::query_as("SELECT id, name from users WHERE name = ?")
            .bind(username)
            .fetch_optional(&self.users)
            .await?;
        Ok(match user {
            None => GetUserResult::CacheMiss,
            Some(LocalUser { id: None, .. }) => GetUserResult::UserDoesNotExists,
            Some(LocalUser { id: Some(id), name }) => GetUserResult::User(User { name, id }),
        })
    }

    pub async fn list_users(&self) -> Result<Vec<User>> {
        let user: Vec<LocalUser> =
            sqlx::query_as("SELECT id, name from users WHERE id IS NOT NULL AND id != ''")
                .fetch_all(&self.users)
                .await?;
        Ok(user
            .into_iter()
            .map(|local| {
                assert!(!local.id.as_ref().unwrap().is_empty());
                User {
                    id: local.id.unwrap(),
                    name: local.name,
                }
            })
            .collect())
    }

    pub async fn store_prefix(&self, prefix: &str, count: i64) -> Result<()> {
        sqlx::query("INSERT INTO prefixes VALUES (?, ?) ON CONFLICT DO NOTHING")
            .bind(prefix)
            .bind(count)
            .execute(&self.prefixes)
            .await?;
        Ok(())
    }

    pub async fn get_prefix(&self, prefix: &str) -> Result<Option<i64>> {
        let count: Option<i64> = sqlx::query_scalar("SELECT count FROM prefixes WHERE prefix = ?")
            .bind(prefix)
            .fetch_optional(&self.prefixes)
            .await?;
        Ok(count)
    }
}
