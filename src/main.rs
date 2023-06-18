use axum::{
    extract::{Query, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use hyper::{header::AUTHORIZATION, HeaderMap, StatusCode};

extern crate redis;
use redis::Commands;
use redis::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use std::{env, sync::Mutex};

use tower_http::cors::CorsLayer;

use uuid::Uuid;

async fn root() -> &'static str {
    "Hello, World!"
}

#[derive(Serialize)]
enum GetAccessTokenResponse {
    Ok(SpotifyAccessTokenResponse),
    Err(String),
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

impl IntoResponse for GetAccessTokenResponse {
    fn into_response(self) -> axum::response::Response {
        match self {
            GetAccessTokenResponse::Ok(result) => (StatusCode::OK, Json(result)).into_response(),
            GetAccessTokenResponse::Err(err) => {
                (StatusCode::UNAUTHORIZED, Json(json!({ "error": err }))).into_response()
            }
        }
    }
}

#[derive(Serialize)]
enum GetRefreshedAccessTokenResponse {
    Ok(SpotifyRefreshedAccessTokenResponse),
    Err(String),
}

#[derive(Serialize, Deserialize, Debug)]
struct SpotifyRefreshedAccessTokenResponse {
    access_token: String,
    token_type: String,
    scope: String,
    expires_in: i32,
}

impl IntoResponse for GetRefreshedAccessTokenResponse {
    fn into_response(self) -> axum::response::Response {
        match self {
            GetRefreshedAccessTokenResponse::Ok(result) => {
                (StatusCode::OK, Json(result)).into_response()
            }
            GetRefreshedAccessTokenResponse::Err(err) => {
                (StatusCode::UNAUTHORIZED, Json(json!({ "error": err }))).into_response()
            }
        }
    }
}

#[derive(Serialize)]
enum SessionTokenResponse {
    Ok(String),
    Err(String),
}

impl IntoResponse for SessionTokenResponse {
    fn into_response(self) -> axum::response::Response {
        match self {
            SessionTokenResponse::Ok(result) => (StatusCode::OK, Json(result)).into_response(),
            SessionTokenResponse::Err(err) => {
                (StatusCode::UNAUTHORIZED, Json(json!({ "error": err }))).into_response()
            }
        }
    }
}

#[derive(Deserialize, Debug)]
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
                GetAccessTokenResponse::Ok(token_res) => token_res,
                GetAccessTokenResponse::Err(e) => return GetAccessTokenResponse::Err(e),
            },
            Err(err) => return GetAccessTokenResponse::Err(err.to_string()),
        };

        // Get user information with the new access token and save profile data
        let user_info = get_user_info(token_response.access_token.to_owned()).await;

        let user_info = match user_info {
            SpotifyUserProfileResponse::Ok(info) => info,
            SpotifyUserProfileResponse::Err(err) => return GetAccessTokenResponse::Err(err),
        };

        let session_id = save_session(&user_info, &state);

        token_response.session = Some(session_id.to_string());

        GetAccessTokenResponse::Ok(token_response)
    } else {
        GetAccessTokenResponse::Err("invalid request".to_string())
    }
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
            GetRefreshedAccessTokenResponse::Ok(res) => res,
            GetRefreshedAccessTokenResponse::Err(e) => {
                return GetRefreshedAccessTokenResponse::Err(e)
            }
        },
        Err(e) => return GetRefreshedAccessTokenResponse::Err(e.to_string()),
    };

    GetRefreshedAccessTokenResponse::Ok(token_response)
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

async fn get_token_body(response: reqwest::Response) -> GetAccessTokenResponse {
    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => return GetAccessTokenResponse::Err(e.to_string()),
    };

    let deserialized: Result<SpotifyAccessTokenResponse, serde_json::Error> =
        serde_json::from_str(&body);

    match deserialized {
        Ok(res) => GetAccessTokenResponse::Ok(res),
        Err(err) => GetAccessTokenResponse::Err(err.to_string()),
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

async fn get_refreshed_token_body(response: reqwest::Response) -> GetRefreshedAccessTokenResponse {
    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => return GetRefreshedAccessTokenResponse::Err(e.to_string()),
    };

    let deserialized: Result<SpotifyRefreshedAccessTokenResponse, serde_json::Error> =
        serde_json::from_str(&body);

    match deserialized {
        Ok(res) => GetRefreshedAccessTokenResponse::Ok(res),
        Err(err) => GetRefreshedAccessTokenResponse::Err(err.to_string()),
    }
}

fn save_session(token_res: &SpotifyUserProfile, state: &AppState) -> Uuid {
    let session_id = Uuid::new_v4();
    let serialized = json!(token_res).to_string();

    let mut con = state.redis_conn.lock().unwrap();
    let _: () = con.set(session_id.to_string(), &serialized).unwrap();

    println!("session saved, key: {} data: {}", session_id, serialized);

    session_id
}

fn get_hashed_header(client_id: &str, client_secret: &str) -> String {
    let plain = format!("{}:{}", client_id, client_secret);
    let base64_encoded = general_purpose::STANDARD_NO_PAD.encode(plain.as_bytes());
    format!("Basic {}", base64_encoded)
}

async fn get_user_info(token: String) -> SpotifyUserProfileResponse {
    let url = "https://api.spotify.com/v1/me";

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse().unwrap());

    let client = reqwest::Client::new();

    let result = client.get(url).headers(headers).send().await;

    let body = match result {
        Ok(res) => match res.text().await {
            Ok(body) => body,
            Err(err) => return SpotifyUserProfileResponse::Err(err.to_string()),
        },
        Err(err) => return SpotifyUserProfileResponse::Err(err.to_string()),
    };

    let parse_result: Result<SpotifyUserProfile, serde_json::Error> = serde_json::from_str(&body);

    match parse_result {
        Ok(profile) => {
            println!("{:?}", profile);
            SpotifyUserProfileResponse::Ok(profile)
        }
        Err(err) => SpotifyUserProfileResponse::Err(err.to_string()),
    }
}

enum SpotifyUserProfileResponse {
    Ok(SpotifyUserProfile),
    Err(String),
}

#[derive(Serialize, Deserialize, Debug)]
struct SpotifyUserProfile {
    country: String,
    display_name: String,
    email: String,
    id: String,
}

#[derive(Clone)]
struct AppState {
    redis_conn: Arc<Mutex<Connection>>,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

#[tokio::main]
async fn main() {
    let client = redis::Client::open("redis://redis").unwrap();

    let con = client.get_connection();

    let redis_conn = match con {
        Ok(c) => {
            println!("redis connection ok");
            c
        }
        Err(err) => {
            panic!("error connecting to redis: {}", err);
        }
    };

    let client_id = env::var("CLIENT_ID").unwrap();
    let client_secret = env::var("CLIENT_SECRET").unwrap();
    let redirect_uri = env::var("REDIRECT_URI").unwrap();

    let shared_state = AppState {
        redis_conn: Arc::new(Mutex::new(redis_conn)),
        client_id,
        client_secret,
        redirect_uri,
    };

    let cors = CorsLayer::permissive();

    let app = Router::new()
        .route("/", get(root))
        .route("/create-session", get(create_session))
        .route("/refresh-token", post(refresh_token))
        .layer(cors)
        .with_state(shared_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    println!("Starting app on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
