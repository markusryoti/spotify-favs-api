use axum::{
    extract::{Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use hyper::{header::AUTHORIZATION, HeaderMap, StatusCode};

extern crate redis;
use redis::Connection;
use redis::{Commands, ToRedisArgs};
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
        let res = client
            .post(url)
            .headers(headers)
            .form(&form_params)
            .send()
            .await;

        match res {
            Ok(response) => {
                let body = match response.text().await {
                    Ok(b) => b,
                    Err(e) => return GetAccessTokenResponse::Err(e.to_string()),
                };

                let token_response: Result<SpotifyAccessTokenResponse, serde_json::Error> =
                    serde_json::from_str(&body);

                match token_response {
                    Ok(res) => {
                        let session_id = Uuid::new_v4();

                        let serialized = json!(&res).to_string();

                        let mut con = state.redis_conn.lock().unwrap();
                        let _: () = con.set(session_id.to_string(), &serialized).unwrap();

                        println!("session saved, key: {} data: {}", session_id, serialized);

                        GetAccessTokenResponse::Ok(res)
                    }
                    Err(err) => GetAccessTokenResponse::Err(err.to_string()),
                }
            }
            Err(err) => GetAccessTokenResponse::Err(err.to_string()),
        }
    } else {
        GetAccessTokenResponse::Err("invalid request".to_string())
    }
}

fn get_hashed_header(client_id: &str, client_secret: &str) -> String {
    let plain = format!("{}:{}", client_id, client_secret);
    let base64_encoded = general_purpose::STANDARD_NO_PAD.encode(plain.as_bytes());
    format!("Basic {}", base64_encoded)
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
        .layer(cors)
        .with_state(shared_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    println!("Starting app on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
