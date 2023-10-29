use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use base64::{engine::general_purpose, Engine as _};
use hyper::{header::AUTHORIZATION, HeaderMap};

#[derive(Serialize, Deserialize, Debug)]
pub struct SpotifyAccessTokenResponse {
    access_token: String,
    token_type: String,
    scope: String,
    expires_in: i32,
    refresh_token: String,
    pub session: Option<String>, // added in the api
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SpotifyRefreshedAccessTokenResponse {
    access_token: String,
    token_type: String,
    scope: String,
    expires_in: i32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SpotifyUserProfile {
    pub country: String,
    pub display_name: String,
    pub email: String,
    pub id: String,
}

#[derive(Deserialize)]
struct RefreshTokenRequest {
    refresh_token: String,
}

pub enum ParseError {
    Err(String),
}

impl ParseError {
    pub fn msg(self) -> String {
        match self {
            ParseError::Err(e) => e,
        }
    }
}

pub enum SpotifyError {
    Err(String),
}

impl SpotifyError {
    pub fn msg(self) -> String {
        match self {
            SpotifyError::Err(e) => e,
        }
    }
}

pub async fn get_token_response(
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
) -> Result<SpotifyAccessTokenResponse, SpotifyError> {
    let token_response = match get_tokens(client_id, client_secret, redirect_uri, code).await {
        Ok(response) => match get_token_body(response).await {
            Ok(token_res) => token_res,
            Err(e) => return Err(SpotifyError::Err(e.msg())),
        },
        Err(err) => return Err(SpotifyError::Err(err.to_string())),
    };

    Ok(token_response)
}

async fn get_tokens(
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = "https://accounts.spotify.com/api/token";

    let client_id = &client_id;
    let client_secret = &client_secret;

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        get_hashed_header(&client_id, &client_secret)
            .parse()
            .unwrap(),
    );

    let redirect_uri = &redirect_uri;

    let mut form_params = HashMap::new();
    form_params.insert("grant_type", "authorization_code");
    form_params.insert("code", &code);
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

pub async fn get_user_info(
    token_response: &SpotifyAccessTokenResponse,
) -> Result<SpotifyUserProfile, ParseError> {
    let url = "https://api.spotify.com/v1/me";

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", token_response.access_token)
            .parse()
            .unwrap(),
    );

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

pub async fn get_refreshed_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<SpotifyRefreshedAccessTokenResponse, SpotifyError> {
    let url = "https://accounts.spotify.com/api/token";

    let client_id = client_id;
    let client_secret = client_secret;

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

    let res = client
        .post(url)
        .headers(headers)
        .form(&form_params)
        .send()
        .await;

    match res {
        Ok(res) => match get_refreshed_token_body(res).await {
            Ok(res) => Ok(res),
            Err(e) => return Err(SpotifyError::Err(e.msg())),
        },
        Err(e) => return Err(SpotifyError::Err(e.to_string())),
    }
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
