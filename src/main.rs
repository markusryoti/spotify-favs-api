use axum::{
    body,
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use hyper::StatusCode;

use serde::{Deserialize, Serialize};
use serde_json::json;

use std::{
    collections::{HashMap, HashSet},
    env,
    net::SocketAddr,
    sync::Arc,
    sync::Mutex,
};

use futures::{sink::SinkExt, stream::StreamExt};
use sqlx::{Pool, Postgres};
use tokio::sync::broadcast::{self, Receiver, Sender};
use tower_http::cors::CorsLayer;

mod db;
mod jwt;
mod spotify;

enum ServerResponse<T> {
    Ok(T),
    Err(StatusCode, String),
}

impl<T> IntoResponse for ServerResponse<T>
where
    T: Serialize,
{
    fn into_response(self) -> axum::response::Response {
        match self {
            ServerResponse::Ok(data) => (StatusCode::OK, Json(data)).into_response(),
            ServerResponse::Err(status, err) => {
                (status, Json(json!({ "error": err }))).into_response()
            }
        }
    }
}

async fn root() -> &'static str {
    "Hello, World!"
}

#[derive(Serialize, Deserialize, Debug)]
struct AccessTokenParams {
    code: String,
    state: String,
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    params: Option<Query<AccessTokenParams>>,
) -> impl IntoResponse {
    if let Some(params) = params {
        let params: AccessTokenParams = params.0;

        let token_result = spotify::get_token_response(
            &state.client_id,
            &state.client_secret,
            &state.redirect_uri,
            &params.code,
        )
        .await;

        let mut token_response = match token_result {
            Ok(res) => res,
            Err(err) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, err.msg()),
        };

        let user_info = spotify::get_user_info(&token_response).await;
        let user_info = match user_info {
            Ok(info) => info,
            Err(err) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, err.msg()),
        };

        // check if user exists, if not then create
        let user_db_response: Result<db::SpotifyUser, sqlx::Error> =
            db::create_user(&state.db_pool, &user_info).await;

        let user = match user_db_response {
            Ok(u) => u,
            Err(e) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };

        let token_result = jwt::generate_jwt(&user);
        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                let values = e.get_error_values();
                return ServerResponse::Err(values.0, values.1);
            }
        };

        token_response.session = Some(token);

        ServerResponse::Ok(token_response)
    } else {
        ServerResponse::Err(StatusCode::BAD_REQUEST, "invalid request".to_string())
    }
}

#[derive(Deserialize)]
struct RefreshTokenRequest {
    refresh_token: String,
}

async fn refresh_token(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RefreshTokenRequest>,
) -> impl IntoResponse {
    let token_response = spotify::get_refreshed_token(
        &state.client_id,
        &state.client_secret,
        &payload.refresh_token,
    )
    .await;

    match token_response {
        Ok(res) => ServerResponse::Ok(res),
        Err(e) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.msg()),
    }
}

#[derive(Deserialize)]
struct AddRoomRequest {
    room_name: String,
}

async fn add_room(
    State(state): State<Arc<AppState>>,
    claims: jwt::Claims,
    Json(payload): Json<AddRoomRequest>,
) -> impl IntoResponse {
    let user_id = claims.sub;
    let user_uid = db::parse_uuid(&user_id);

    if user_uid.is_err() {
        return ServerResponse::Err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "error converting user id to uuid".to_string(),
        );
    }

    let user_uid = user_uid.unwrap();

    let room = db::Room {
        id: None,
        name: payload.room_name,
        owner: user_uid,
        created_at: None,
    };

    let res = db::add_room(&state.db_pool, &room).await;

    match res {
        Ok(room) => ServerResponse::Ok(room),
        Err(e) => ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn rooms_by_user(
    State(state): State<Arc<AppState>>,
    claims: jwt::Claims,
) -> impl IntoResponse {
    let user_id = claims.sub;

    let rooms = db::get_rooms_by_user(&state.db_pool, &user_id).await;

    match rooms {
        Ok(rooms) => ServerResponse::Ok(rooms),
        Err(e) => ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.msg()),
    }
}

async fn room_by_id(
    Path(room_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let room = db::get_room(&state.db_pool, &room_id).await;

    let room = match room {
        Ok(r) => r,
        Err(e) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.msg()),
    };

    if let Some(room) = room {
        ServerResponse::Ok(room)
    } else {
        ServerResponse::Err(StatusCode::NOT_FOUND, "room not found".to_string())
    }
}

struct ConnectedRooms {
    rooms: Mutex<HashMap<String, Room>>, // key - room id
}

impl ConnectedRooms {
    fn new() -> Self {
        ConnectedRooms {
            rooms: Mutex::new(HashMap::new()),
        }
    }
}

struct Room {
    tx: broadcast::Sender<String>,
    tracks: Mutex<HashMap<String, Track>>,
    users: Mutex<HashSet<String>>,
}

impl Room {
    fn new(tx: broadcast::Sender<String>) -> Self {
        Room {
            tx,
            tracks: Mutex::new(HashMap::new()),
            users: Mutex::new(HashSet::new()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct WsParams {
    access_token: String,
}

async fn ws_handler(
    Path(room_id): Path<String>,
    params: Query<WsParams>,
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!(
        "New ws request for room: {}, token: {}",
        room_id, params.access_token
    );

    let claims = match jwt::decode_jwt(&params.access_token) {
        Ok(claims) => claims,
        Err(e) => {
            println!("Claims decode error: {:?}", e);

            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(body::boxed(body::Empty::new()))
                .unwrap();
        }
    };

    let user_id = claims.sub;

    let user = db::get_user(&state.db_pool, &user_id).await;
    let user = match user {
        Ok(u) => u,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(body::boxed(body::Empty::new()))
                .unwrap()
        }
    };

    let user = user.unwrap();

    let room = db::get_room(&state.db_pool, &room_id).await;
    let room = match room {
        Ok(r) => r,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(body::boxed(body::Empty::new()))
                .unwrap()
        }
    };

    if let Some(_room) = room {
        insert_pool(&state, &room_id);

        ws.on_upgrade(move |socket| handle_socket(socket, state, room_id.clone(), user))
    } else {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(body::boxed(body::Empty::new()))
            .unwrap()
    }
}

fn insert_pool(state: &AppState, room_id: &str) {
    let mut locked_pools = state.connected.rooms.lock().unwrap();

    if !locked_pools.contains_key(room_id) {
        let (tx, _rx) = broadcast::channel(100);

        let new_pool = Room::new(tx);

        locked_pools.insert(room_id.to_owned(), new_pool);
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct WsMessage {
    track: Track,
    user_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Track {
    id: String,
    uri: String,
    name: String,
    artists: Vec<Artist>,
    album: Album,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Album {
    images: Vec<AlbumImage>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AlbumImage {
    url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Artist {
    name: String,
}

async fn handle_socket(
    stream: WebSocket,
    state: Arc<AppState>,
    room_id: String,
    user: db::SpotifyUser,
) {
    let (mut sender, mut receiver) = stream.split();

    let mut rx = get_receiver(&state, &room_id);

    // Spawn the first task that will receive broadcast messages and send text
    // messages over the websocket to our client.
    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            // In any websocket error, break loop.
            if sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    let tx = get_sender(&state, &room_id);

    let user_id = user.id.unwrap().to_string();

    let state_clone = state.clone();
    let room_id_clone = room_id.clone();
    let user_clone = user.clone();

    // Spawn a task that takes messages from the websocket and sends them to all broadcast subscribers.
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            let deserialized: Result<WsMessage, serde_json::Error> = serde_json::from_str(&text);
            let mut msg = deserialized.unwrap();

            msg.user_id = Some(user_id.clone());

            update_room(
                &state_clone,
                &room_id_clone,
                &user_clone.display_name,
                &msg.track,
            );

            let room_data = get_room(&state_clone, &room_id_clone);
            let serialized = serde_json::to_string(&room_data).unwrap();
            let _ = tx.send(serialized);
        }
    });

    // If any one of the tasks run to completion, we abort the other.
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };

    remove_user_from_room(&state, &room_id, &user.display_name)
}

fn get_receiver(state: &AppState, room_id: &str) -> Receiver<String> {
    let room_map = state.connected.rooms.lock().unwrap();
    let room_pool = room_map.get(room_id).unwrap();
    let rx = room_pool.tx.subscribe();

    rx
}

fn get_sender(state: &AppState, room_id: &str) -> Sender<String> {
    // Clone things we want to pass (move) to the receiving task.
    let room_map = state.connected.rooms.lock().unwrap();
    let room_pool = room_map.get(room_id).unwrap();
    let tx = room_pool.tx.clone();

    tx
}

fn update_room(state: &AppState, room_id: &str, display_name: &str, track: &Track) {
    let room_map = state.connected.rooms.lock().unwrap();
    let room = room_map.get(room_id).unwrap();

    room.tracks
        .lock()
        .unwrap()
        .insert(display_name.to_string(), track.clone());

    println!("User {} added to room: {}", display_name, room_id);
}

fn remove_user_from_room(state: &AppState, room_id: &str, display_name: &str) {
    let room_map = state.connected.rooms.lock().unwrap();
    let room = room_map.get(room_id).unwrap();

    room.tracks.lock().unwrap().remove(display_name);

    println!("User {} removed from room: {}", display_name, room_id);
}

fn get_room(state: &AppState, room_id: &str) -> HashMap<String, Track> {
    let room_map = state.connected.rooms.lock().unwrap();

    let tracks = room_map
        .get(room_id)
        .unwrap()
        .tracks
        .lock()
        .unwrap()
        .clone();

    tracks
}

struct AppState {
    db_pool: Pool<Postgres>,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    connected: ConnectedRooms,
}

#[tokio::main]
async fn main() {
    let pg_db = db::Client::new().await;

    let client_id = env::var("CLIENT_ID").unwrap();
    let client_secret = env::var("CLIENT_SECRET").unwrap();
    let redirect_uri = env::var("REDIRECT_URI").unwrap();

    let connected = ConnectedRooms::new();

    let shared_state = Arc::new(AppState {
        db_pool: pg_db.conn(),
        client_id,
        client_secret,
        redirect_uri,
        connected,
    });

    let cors = CorsLayer::permissive();

    let app = Router::new()
        .route("/", get(root))
        .route("/create-session", get(create_session))
        .route("/refresh-token", post(refresh_token))
        .route("/add-room", post(add_room))
        .route("/rooms", get(rooms_by_user))
        .route("/rooms/:id", get(room_by_id))
        .route("/rooms/:id/ws", get(ws_handler))
        .layer(cors)
        .with_state(shared_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    println!("Starting app on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
