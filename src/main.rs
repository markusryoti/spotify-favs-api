use axum::{
    extract::{ws::WebSocket, Path, Query, State, WebSocketUpgrade},
    headers::Server,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use base64::{engine::general_purpose, Engine as _};
use hyper::{header::AUTHORIZATION, HeaderMap, StatusCode};

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::{collections::HashMap, net::SocketAddr};

use tower_http::cors::CorsLayer;

use sqlx::{Pool, Postgres};

mod db;
mod jwt;

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

enum ParseError {
    Err(String),
}

impl ParseError {
    fn msg(self) -> String {
        match self {
            ParseError::Err(e) => e,
        }
    }
}

async fn root() -> &'static str {
    "Hello, World!"
}

#[derive(Serialize, Deserialize, Debug)]
struct SpotifyAccessTokenResponse {
    access_token: String,
    token_type: String,
    scope: String,
    expires_in: i32,
    refresh_token: String,
    session: Option<String>, // added in the api
}

#[derive(Serialize, Deserialize, Debug)]
struct SpotifyRefreshedAccessTokenResponse {
    access_token: String,
    token_type: String,
    scope: String,
    expires_in: i32,
}

#[derive(Serialize, Deserialize, Debug)]
struct AccessTokenParams {
    code: String,
    state: String,
}

async fn create_session(
    State(state): State<AppState>,
    params: Option<Query<AccessTokenParams>>,
) -> impl IntoResponse {
    if let Some(params) = params {
        let params: AccessTokenParams = params.0;

        let mut token_response = match get_tokens(&state, params).await {
            Ok(response) => match get_token_body(response).await {
                Ok(token_res) => token_res,
                Err(e) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.msg()),
            },
            Err(err) => {
                return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
            }
        };

        // Get user information with the new access token and save profile data
        let user_info = get_user_info(token_response.access_token.to_owned()).await;
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

        let token_result = generate_jwt(&user);
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

fn generate_jwt(user: &db::SpotifyUser) -> Result<String, jwt::AuthError> {
    let claims = jwt::Claims {
        sub: user.id.unwrap().to_string(),
        display_name: user.display_name.to_string(),
        exp: 2000000000,
    };

    jwt::encode_claims(claims)
}

#[derive(Deserialize)]
struct RefreshTokenRequest {
    refresh_token: String,
}

async fn refresh_token(
    State(state): State<AppState>,
    Json(payload): Json<RefreshTokenRequest>,
) -> impl IntoResponse {
    let token_response = match get_refreshed_token(&state, payload.refresh_token).await {
        Ok(res) => match get_refreshed_token_body(res).await {
            Ok(res) => res,
            Err(e) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.msg()),
        },
        Err(e) => return ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    ServerResponse::Ok(token_response)
}

async fn get_tokens(
    state: &AppState,
    params: AccessTokenParams,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = "https://accounts.spotify.com/api/token";

    let client_id = &state.client_id;
    let client_secret = &state.client_secret;

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        get_hashed_header(&client_id, &client_secret)
            .parse()
            .unwrap(),
    );

    let redirect_uri = &state.redirect_uri;

    let mut form_params = HashMap::new();
    form_params.insert("grant_type", "authorization_code");
    form_params.insert("code", &params.code);
    form_params.insert("redirect_uri", &redirect_uri);

    let client = reqwest::Client::new();

    client
        .post(url)
        .headers(headers)
        .form(&form_params)
        .send()
        .await
}

async fn get_token_body(
    response: reqwest::Response,
) -> Result<SpotifyAccessTokenResponse, ParseError> {
    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => return Err(ParseError::Err(e.to_string())),
    };

    let deserialized: Result<SpotifyAccessTokenResponse, serde_json::Error> =
        serde_json::from_str(&body);

    match deserialized {
        Ok(res) => Ok(res),
        Err(err) => Err(ParseError::Err(err.to_string())),
    }
}

async fn get_refreshed_token(
    state: &AppState,
    refresh_token: String,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = "https://accounts.spotify.com/api/token";

    let client_id = &state.client_id;
    let client_secret = &state.client_secret;

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        get_hashed_header(&client_id, &client_secret)
            .parse()
            .unwrap(),
    );

    let mut form_params = HashMap::new();
    form_params.insert("grant_type", "refresh_token");
    form_params.insert("refresh_token", &refresh_token);

    let client = reqwest::Client::new();

    client
        .post(url)
        .headers(headers)
        .form(&form_params)
        .send()
        .await
}

async fn get_refreshed_token_body(
    response: reqwest::Response,
) -> Result<SpotifyRefreshedAccessTokenResponse, ParseError> {
    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => return Err(ParseError::Err(e.to_string())),
    };

    let deserialized: Result<SpotifyRefreshedAccessTokenResponse, serde_json::Error> =
        serde_json::from_str(&body);

    match deserialized {
        Ok(res) => Ok(res),
        Err(err) => Err(ParseError::Err(err.to_string())),
    }
}

fn get_hashed_header(client_id: &str, client_secret: &str) -> String {
    let plain = format!("{}:{}", client_id, client_secret);
    let base64_encoded = general_purpose::STANDARD_NO_PAD.encode(plain.as_bytes());
    format!("Basic {}", base64_encoded)
}

async fn get_user_info(token: String) -> Result<SpotifyUserProfile, ParseError> {
    let url = "https://api.spotify.com/v1/me";

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse().unwrap());

    let client = reqwest::Client::new();

    let result = client.get(url).headers(headers).send().await;

    let body = match result {
        Ok(res) => match res.text().await {
            Ok(body) => body,
            Err(err) => return Err(ParseError::Err(err.to_string())),
        },
        Err(err) => return Err(ParseError::Err(err.to_string())),
    };

    let parse_result: Result<SpotifyUserProfile, serde_json::Error> = serde_json::from_str(&body);

    match parse_result {
        Ok(profile) => {
            println!("{:?}", profile);
            Ok(profile)
        }
        Err(err) => Err(ParseError::Err(err.to_string())),
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SpotifyUserProfile {
    country: String,
    display_name: String,
    email: String,
    id: String,
}

#[derive(Deserialize)]
struct AddRoomRequest {
    room_name: String,
}

async fn add_room(
    State(state): State<AppState>,
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

async fn rooms_by_user(State(state): State<AppState>, claims: jwt::Claims) -> impl IntoResponse {
    let user_id = claims.sub;

    let rooms = db::get_rooms_by_user(&state.db_pool, &user_id).await;

    match rooms {
        Ok(rooms) => ServerResponse::Ok(rooms),
        Err(e) => ServerResponse::Err(StatusCode::INTERNAL_SERVER_ERROR, e.msg()),
    }
}

async fn room_by_id(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
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

async fn ws_handler(Path(room_id): Path<String>, ws: WebSocketUpgrade) -> Response {
    println!("New ws request for room: {}", room_id);
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    while let Some(msg) = socket.recv().await {
        let msg = if let Ok(msg) = msg {
            msg
        } else {
            // client disconnected
            return;
        };

        println!("{:?}", msg);

        if socket.send(msg).await.is_err() {
            // client disconnected
            return;
        }
    }
}

#[derive(Clone)]
struct AppState {
    db_pool: Pool<Postgres>,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

#[tokio::main]
async fn main() {
    let pg_db = db::Client::new().await;

    let client_id = env::var("CLIENT_ID").unwrap();
    let client_secret = env::var("CLIENT_SECRET").unwrap();
    let redirect_uri = env::var("REDIRECT_URI").unwrap();

    let shared_state = AppState {
        db_pool: pg_db.conn(),
        client_id,
        client_secret,
        redirect_uri,
    };

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
