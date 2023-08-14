use serde::{Deserialize, Serialize};
use sqlx::Postgres;

use crate::SpotifyUserProfile;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize)]
pub struct SpotifyUser {
    pub id: i32,
    pub spotify_user_id: String,
    pub display_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct Room {
    pub id: u32,
    pub owner: u32,
    pub name: String,
    pub created_at: String,
}

pub async fn create_user(
    pool: &sqlx::Pool<Postgres>,
    user: &SpotifyUserProfile,
) -> Result<SpotifyUser, sqlx::Error> {
    let spotify_id = user.id.clone();
    let existing_result = get_user(pool, &spotify_id).await;

    match existing_result {
        Ok(user_opt) => match user_opt {
            Some(user) => Ok(user),
            None => {
                let new_user = create_new_user(pool, &spotify_id, &user.display_name).await?;

                Ok(new_user)
            }
        },
        Err(e) => Err(e),
    }
}

async fn get_user(
    pool: &sqlx::Pool<Postgres>,
    id: &str,
) -> Result<Option<SpotifyUser>, sqlx::Error> {
    let res =
        sqlx::query_as::<_, SpotifyUser>("SELECT * FROM spotify_user WHERE spotify_user_id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

    match res {
        Some(user) => {
            println!("Found user: {:?}", user);
            Ok(Some(user))
        }
        None => Ok(None),
    }
}

async fn create_new_user(
    pool: &sqlx::Pool<Postgres>,
    id: &str,
    display_name: &str,
) -> Result<SpotifyUser, sqlx::Error> {
    println!("no user, creating");

    let insert_result =
        sqlx::query("INSERT INTO spotify_user(spotify_user_id, display_name) VALUES ($1, $2)")
            .bind(id)
            .bind(display_name)
            .execute(pool)
            .await?;

    if insert_result.rows_affected() > 0 {
        println!("new user created: {}", id);

        let created_opt = get_user(pool, id).await?;

        if let Some(u) = created_opt {
            Ok(u)
        } else {
            return Err(sqlx::Error::RowNotFound);
        }
    } else {
        return Err(sqlx::Error::RowNotFound);
    }
}
