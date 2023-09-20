use crate::SpotifyUserProfile;
use serde::Serialize;
use sqlx::types::uuid;
use sqlx::types::uuid::Uuid;
use sqlx::Postgres;

use sqlx::{postgres::PgPoolOptions, Pool};

pub struct Client {
    pool: Pool<Postgres>,
}

impl Client {
    pub async fn new() -> Self {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect("postgres://devuser:devpassword@db:5432/spotify_favorites")
            .await
            .unwrap();

        Client { pool }
    }

    pub fn conn(self) -> Pool<Postgres> {
        self.pool
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct SpotifyUser {
    pub id: Option<Uuid>,
    pub spotify_user_id: String,
    pub display_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Room {
    pub id: Option<Uuid>,
    pub owner: Uuid,
    pub name: String,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug)]
pub enum DbErr {
    Call(sqlx::Error),
    Uuid(uuid::Error),
}

impl DbErr {
    pub fn msg(self) -> String {
        match self {
            DbErr::Call(e) => e.to_string(),
            DbErr::Uuid(e) => e.to_string(),
        }
    }
}

pub async fn create_user(
    pool: &sqlx::Pool<Postgres>,
    user: &SpotifyUserProfile,
) -> Result<SpotifyUser, sqlx::Error> {
    let spotify_id = user.id.clone();
    let existing_result = get_user(pool, &spotify_id).await?;

    let user_res = match existing_result {
        Some(user) => return Ok(user),
        None => {
            insert_user(pool, &spotify_id, &user.display_name).await?;

            let user_res = get_user(pool, &spotify_id).await?;
            if let Some(u) = user_res {
                Ok(u)
            } else {
                Err(sqlx::Error::RowNotFound)
            }
        }
    };

    user_res
}

async fn get_user(
    pool: &sqlx::Pool<Postgres>,
    spotify_user_id: &str,
) -> Result<Option<SpotifyUser>, sqlx::Error> {
    let res =
        sqlx::query_as::<_, SpotifyUser>("SELECT * FROM spotify_user WHERE spotify_user_id = $1")
            .bind(spotify_user_id)
            .fetch_optional(pool)
            .await?;

    if let Some(user) = res {
        println!("Found user: {:?}", user);
        Ok(Some(user))
    } else {
        println!("Didn't find user with id: {}", spotify_user_id);
        Ok(None)
    }
}

async fn insert_user(
    pool: &sqlx::Pool<Postgres>,
    spotify_user_id: &str,
    display_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO spotify_user(spotify_user_id, display_name) VALUES ($1, $2)")
        .bind(spotify_user_id)
        .bind(display_name)
        .execute(pool)
        .await?;

    println!("new user created: {}", spotify_user_id);
    Ok(())
}

pub fn parse_uuid(uuid_str: &str) -> Result<Uuid, uuid::Error> {
    Uuid::parse_str(uuid_str)
}

pub async fn add_room(pool: &sqlx::Pool<Postgres>, room: &Room) -> Result<Room, sqlx::Error> {
    let existing_result = get_room_with_name_and_user(pool, &room.owner, &room.name).await?;

    if let Some(existing_room) = existing_result {
        Ok(existing_room)
    } else {
        insert_room(pool, &room).await?;

        let room = get_room_with_name_and_user(pool, &room.owner, &room.name).await?;

        if let Some(u) = room {
            Ok(u)
        } else {
            Err(sqlx::Error::RowNotFound)
        }
    }
}

async fn insert_room(pool: &sqlx::Pool<Postgres>, room: &Room) -> Result<(), sqlx::Error> {
    let room_name = &room.name;

    sqlx::query("INSERT INTO room(name, owner) VALUES ($1, $2)")
        .bind(&room_name)
        .bind(&room.owner)
        .execute(pool)
        .await?;

    println!("new room created: {}", room_name);

    Ok(())
}

pub async fn get_room(pool: &sqlx::Pool<Postgres>, room_id: &str) -> Result<Option<Room>, DbErr> {
    let uuid = parse_uuid(room_id);

    let uuid = match uuid {
        Ok(id) => id,
        Err(e) => return Err(DbErr::Uuid(e)),
    };

    let res = sqlx::query_as::<_, Room>("SELECT * FROM room WHERE id = $1")
        .bind(uuid)
        .fetch_optional(pool)
        .await;

    let room = match res {
        Ok(room_opt) => room_opt,
        Err(e) => return Err(DbErr::Call(e)),
    };

    if let Some(room) = room {
        println!("Found room: {:?}", room);
        Ok(Some(room))
    } else {
        println!("Didn't find room with id: {}", room_id);
        Ok(None)
    }
}

async fn get_room_with_name_and_user(
    pool: &sqlx::Pool<Postgres>,
    user_id: &Uuid,
    room_name: &str,
) -> Result<Option<Room>, sqlx::Error> {
    let res = sqlx::query_as::<_, Room>("SELECT * FROM room WHERE owner = $1 AND name = $2")
        .bind(user_id)
        .bind(room_name)
        .fetch_optional(pool)
        .await?;

    if let Some(room) = res {
        println!("Found room: {:?}", room);
        Ok(Some(room))
    } else {
        println!("Didn't find room with name: {}", room_name);
        Ok(None)
    }
}

pub async fn get_rooms_by_user(
    pool: &sqlx::Pool<Postgres>,
    user_id: &str,
) -> Result<Vec<Room>, DbErr> {
    let uuid = parse_uuid(user_id);

    let uuid = match uuid {
        Ok(id) => id,
        Err(e) => return Err(DbErr::Uuid(e)),
    };

    let stream = sqlx::query_as::<_, Room>("SELECT * FROM room WHERE owner = $1")
        .bind(uuid)
        .fetch_all(pool)
        .await;

    let stream = match stream {
        Ok(s) => s,
        Err(e) => return Err(DbErr::Call(e)),
    };

    let rooms: Vec<Room> = stream.into_iter().collect();

    Ok(rooms)
}
