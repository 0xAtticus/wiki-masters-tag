use anyhow::anyhow;
use std::fmt::Display;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use anyhow::Result;
use base64::prelude::*;
use chrono::Utc;
use hex_color::HexColor;
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::value::Value;

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Card {
    pub id: String,
    pub wikipedia_url: String, // flemme
    pub wikipedia_title: String,
    pub rarity: CardRarity,
}

#[derive(Deserialize, Debug, clap::ValueEnum, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum CardRarity {
    L,
    UR,
    SR,
    R,
    PC,
    C,
}

impl Display for CardRarity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CardRarity::L => "L",
            CardRarity::UR => "UR",
            CardRarity::SR => "SR",
            CardRarity::R => "R",
            CardRarity::PC => "PC",
            CardRarity::C => "C",
        };
        write!(f, "{}", s)
    }
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    color: String,
    user_id: String,
}

#[derive(Deserialize, Debug)]
pub struct CollectedCard {
    pub id: String,
    pub tags: Vec<Tag>,
    pub card: Card,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CollectionPage {
    pub collection: Vec<CollectedCard>,
    pub total: Option<usize>,
}

pub struct WikiMasterApi {
    app_cookie: Arc<RwLock<String>>,
    supabase_api_key: String,
    supabase_bearer: Arc<RwLock<String>>,
    user_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct User {
    pub name: String,
    pub id: String,
}

macro_rules! api_url {
    ($path:expr) => {
        concat!("https://www.wiki-masters.com/api/", $path)
    };
}

macro_rules! supabase_url {
    ($path:expr) => {
        concat!("https://cyrxjeppjqsxxjayfrur.supabase.co/rest/v1/", $path)
    };
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TagId(String);

#[derive(Serialize)]
pub struct AttachTagRequest<'a> {
    pub tag_id: TagId,
    pub user_card_id: &'a str,
}

pub struct CreateTagRequest<'a> {
    pub name: &'a str,
}

#[derive(Serialize)]
pub struct DeleteTagRequest<'a> {
    pub tag_id: &'a TagId,
}

#[derive(Serialize)]
pub struct DeleteTagFromCardRequest<'a> {
    pub user_card_id: &'a str,
}

pub struct SetTagColorRequest {
    pub color: HexColor,
    pub tag_id: TagId,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Auction {
    pub id: String,
    seller_id: String,
    card_id: String,
    pub effective_bid: usize,
    pub status: String,
    pub end_at: chrono::DateTime<Utc>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Marketplace {
    pub auctions: Vec<Auction>,
}

#[derive(Debug)]
pub enum CollectionResponse {
    Private,
    ProfileDoesNotExist,
    Public { collection: CollectionPage },
}

impl WikiMasterApi {
    // Return api and user id
    pub async fn from_credentials(
        email: &str,
        password: &str,
        supabase_api_key: String,
    ) -> Result<WikiMasterApi> {
        let client = reqwest::Client::new();
        let url = reqwest::Url::parse_with_params(
            "https://cyrxjeppjqsxxjayfrur.supabase.co/auth/v1/token",
            &[("grant_type", "password")],
        )?;

        let response_text: String = client
            .post(url)
            .json(&json!({
                "email": email,
                "password": password,
                "gotrue_meta_security": {}
            }))
            .header("apikey", supabase_api_key.clone())
            .send()
            .await?
            .text()
            .await?;
        let response: Value = serde_json::from_str(&response_text).unwrap();
        let app_cookie_b64 = BASE64_STANDARD_NO_PAD
            .encode(response_text.as_bytes())
            .to_string();
        let refresh_token = response
            .get("refresh_token")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let (cookie_part1, cookie_part2) = app_cookie_b64.split_at(3173);
        let supabase_bearer = Arc::new(RwLock::new(
            response
                .get("access_token")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
        ));
        let app_cookie = Arc::new(RwLock::new(format!(
            "sb-cyrxjeppjqsxxjayfrur-auth-token.0=base64-{cookie_part1}; sb-cyrxjeppjqsxxjayfrur-auth-token.1={cookie_part2}"
        )));

        {
            let supabase_api_key = supabase_api_key.clone();
            let app_cookie = app_cookie.clone();
            let supabase_bearer = supabase_bearer.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_mins(30)).await;
                    let client = reqwest::Client::new();
                    let url = reqwest::Url::parse_with_params(
                        "https://cyrxjeppjqsxxjayfrur.supabase.co/auth/v1/token",
                        &[("grant_type", "refresh_token")],
                    )
                    .unwrap();
                    let response_text: String = client
                        .post(url)
                        .json(&json!({
                            "token": &refresh_token,
                            "gotrue_meta_security": {}
                        }))
                        .header("apikey", supabase_api_key.clone())
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap();

                    let response: Value = serde_json::from_str(&response_text).unwrap();
                    let app_cookie_b64 = BASE64_STANDARD_NO_PAD
                        .encode(response_text.as_bytes())
                        .to_string();
                    let (cookie_part1, cookie_part2) = app_cookie_b64.split_at(3173);
                    {
                        let mut ptr = app_cookie.write().await;
                        *ptr = format!(
                            "sb-cyrxjeppjqsxxjayfrur-auth-token.0=base64-{cookie_part1}; sb-cyrxjeppjqsxxjayfrur-auth-token.1={cookie_part2}"
                        );
                    }

                    {
                        let mut ptr = supabase_bearer.write().await;
                        *ptr = response
                            .get("access_token")
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_string();
                    }
                }
            });
        }
        let api = WikiMasterApi {
            supabase_api_key,
            supabase_bearer,
            app_cookie,
            user_id: response
                .get("user")
                .unwrap()
                .get("id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
        };

        Ok(api)
    }

    pub async fn my_collection(
        &self,
        page: usize,
        tag_id: Option<&TagId>,
    ) -> Result<CollectionPage> {
        let client = reqwest::Client::new();
        let mut params = vec![
            ("page", page.to_string()),
            ("sort", "added".to_string()), // Sort by most recent
            ("pending", "1".to_string()),
        ];
        if let Some(tag) = tag_id {
            params.push(("tag_id", tag.0.clone()));
        }
        let url = reqwest::Url::parse_with_params(api_url!("my-collection"), &params)?;
        let response = client
            .get(url)
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .send()
            .await?;
        Ok(response.json().await?)
    }

    pub async fn attach_tag<'a>(&self, request: AttachTagRequest<'a>) -> Result<()> {
        let client = reqwest::Client::new();
        client
            .post(supabase_url!("user_card_tags"))
            .bearer_auth(self.supabase_bearer.read().await.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .json(&request)
            .send()
            .await?;
        Ok(())
    }

    pub async fn create_tag<'a>(&self, request: CreateTagRequest<'a>) -> Result<()> {
        let client = reqwest::Client::new();
        let response = client
            .post(supabase_url!("tags"))
            .bearer_auth(self.supabase_bearer.read().await.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .json(&json!({
                "name": request.name,
                "user_id": self.user_id
            }))
            .send()
            .await?;
        tracing::debug!("create tag response: {}", response.text().await?);
        Ok(())
    }

    pub async fn delete_tag<'a>(&self, request: DeleteTagRequest<'a>) -> Result<()> {
        tracing::debug!("Deleting tag {}", request.tag_id.0);
        let client = reqwest::Client::new();
        let url = reqwest::Url::parse_with_params(
            supabase_url!("tags"),
            &[("id", &format!("eq.{}", request.tag_id.0))],
        )?;
        client
            .delete(url)
            .bearer_auth(self.supabase_bearer.read().await.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .send()
            .await?;
        Ok(())
    }

    pub async fn delete_tag_from_card<'a>(
        &self,
        request: DeleteTagFromCardRequest<'a>,
    ) -> Result<()> {
        let client = reqwest::Client::new();
        let url = reqwest::Url::parse_with_params(
            supabase_url!("user_card_tags"),
            &[("user_card_id", &format!("eq.{}", request.user_card_id))],
        )?;
        client
            .delete(url)
            .bearer_auth(self.supabase_bearer.read().await.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .send()
            .await?;
        Ok(())
    }

    pub async fn list_tags(&self) -> Result<Vec<Tag>> {
        let client = reqwest::Client::new();
        let url = reqwest::Url::parse_with_params(
            supabase_url!("tags"),
            &[
                ("select", "*"),
                ("user_id", &format!("eq.{}", self.user_id)),
            ],
        )?;

        let response = client
            .get(url)
            .bearer_auth(self.supabase_bearer.read().await.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .send()
            .await?;
        Ok(response.json().await?)
    }

    pub async fn set_tag_color(&self, request: SetTagColorRequest) -> Result<()> {
        #[derive(Serialize)]
        struct Color {
            color: HexColor,
        }
        let client = reqwest::Client::new();
        let url = reqwest::Url::parse_with_params(
            supabase_url!("tags"),
            &[("id", &format!("eq.{}", request.tag_id.0))],
        )?;

        client
            .patch(url)
            .bearer_auth(self.supabase_bearer.read().await.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .json(&Color {
                color: request.color,
            })
            .send()
            .await?;
        Ok(())
    }

    pub async fn friends(&self) -> Result<Vec<User>> {
        let client = reqwest::Client::new();
        #[derive(Deserialize)]
        struct Response {
            friendships: Vec<FriendShip>,
        }
        #[derive(Deserialize)]
        struct LocalUser {
            id: String,
            username: String,
        }
        #[derive(Deserialize)]
        struct FriendShip {
            requester: LocalUser,
            addressee: LocalUser,
        }
        let response: Response = client
            .get(api_url!("friends"))
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .send()
            .await?
            .json()
            .await?;
        Ok(response
            .friendships
            .into_iter()
            .map(|friendship| {
                if friendship.addressee.id == self.user_id {
                    friendship.requester
                } else {
                    friendship.addressee
                }
            })
            .map(|local_user| User {
                id: local_user.id,
                name: local_user.username,
            })
            .collect())
    }

    pub async fn create_trade(&self, card_ids: &[&str], recipient_id: &str) -> Result<()> {
        let client = reqwest::Client::new();
        #[derive(Serialize)]
        struct Item<'a> {
            offered_by: &'a str,
            card_id: &'a str,
        }
        #[derive(Serialize)]
        struct Trade<'a> {
            recipient_id: &'a str,
            items: Vec<Item<'a>>,
        }
        let response = client
            .post(api_url!("trades"))
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .json(&Trade {
                recipient_id,
                items: card_ids
                    .iter()
                    .map(|card_id| Item {
                        card_id,
                        offered_by: &self.user_id,
                    })
                    .collect(),
            })
            .send()
            .await?
            .text()
            .await?;
        tracing::debug!("Create trade response {response}");
        Ok(())
    }

    pub async fn sell_card(
        &self,
        card_id: &str,
        duration_minutes: usize,
        price: usize,
    ) -> Result<()> {
        let client = reqwest::Client::new();
        let response = client
            .post(api_url!("marketplace"))
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .json(&json!({
                "base_amount": price,
                "duration_minutes": duration_minutes,
                "card_id": card_id
            }))
            .send()
            .await?
            .text()
            .await?;
        tracing::debug!("Sell response {response}");

        Ok(())
    }

    pub async fn bid(&self, bid_id: String, wikibidous: usize) -> Result<()> {
        let endpoint = format!(
            "https://www.wiki-masters.com/api/marketplace/{}/bid",
            bid_id
        );
        let client = reqwest::Client::new();
        let response = client
            .post(endpoint)
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .json(&json!({
                "amount": wikibidous
            }))
            .send()
            .await?
            .text()
            .await?;
        tracing::debug!("Bid response {response}");

        Ok(())
    }

    pub async fn marketplace(&self, limit: usize, page: NonZeroUsize) -> Result<Marketplace> {
        let client = reqwest::Client::new();
        let url = reqwest::Url::parse_with_params(
            api_url!("marketplace"),
            &[
                ("page", page.to_string()),
                ("limit", limit.to_string()),
                ("sort", "ending_soon".to_string()),
            ],
        )?;
        let marketplace = client
            .get(url)
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .send()
            .await?
            .json()
            .await?;
        tracing::info!("{:?}", marketplace);
        Ok(marketplace)
    }

    pub async fn search_users(&self, prefix: &str) -> Result<Vec<User>> {
        assert!(prefix.len() >= 2);
        #[derive(Deserialize, Debug)]
        struct UserLocal {
            username: String,
            id: String,
        }
        #[derive(Deserialize)]
        struct Response {
            users: Vec<UserLocal>,
        }
        let url = reqwest::Url::parse_with_params(api_url!("friends/search"), &[("q", &prefix)])?;
        let client = reqwest::Client::new();
        tracing::debug!("Querying prefix: {prefix}");
        Ok(client
            .get(url)
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .send()
            .await?
            .json::<Response>()
            .await?
            .users
            .into_iter()
            .map(|local| User {
                name: local.username,
                id: local.id,
            })
            .collect())
    }

    pub async fn get_user(&self, user_name: &str) -> Result<Option<User>> {
        #[derive(Deserialize, Debug)]
        struct UserLocal {
            username: String,
            id: String,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Response {
            Error { error: String },
            UserProfile { profile: UserLocal },
        }
        if user_name.contains('%') {
            return Ok(None);
        }

        let client = reqwest::Client::new();
        let url = &format!("https://www.wiki-masters.com/api/profile/{user_name}");
        let result: Response = client
            .get(url)
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .send()
            .await?
            .json()
            .await?;
        Ok(match result {
            Response::Error { .. } => None,
            Response::UserProfile { profile } => Some(User {
                name: profile.username,
                id: profile.id,
            }),
        })
    }

    pub async fn user_collection(
        &self,
        username: &str,
        page: usize,
        card_title: Option<&str>,
    ) -> Result<CollectionResponse> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Response {
            Error { error: String },
            Page(CollectionPage),
        }
        let url = reqwest::Url::parse_with_params(
            &format!("https://www.wiki-masters.com/api/profile/{username}/collection"),
            vec![
                ("page", page.to_string()),
                (
                    "q",
                    card_title.map(|v| v.to_string()).unwrap_or(String::new()),
                ),
                ("sort", "rarity".to_string()),
                ("pending", 1.to_string()),
            ]
            .iter()
            .filter(|(_, v)| !v.is_empty()),
        )?;
        let client = reqwest::Client::new();
        let result: Response = client
            .get(url)
            .header(header::COOKIE, &*self.app_cookie.read().await)
            .send()
            .await?
            .json()
            .await?;

        match result {
            Response::Error { error } if error == "Collection privée" => {
                Ok(CollectionResponse::Private)
            }
            Response::Error { error } if error == "Profil introuvable" => {
                Ok(CollectionResponse::ProfileDoesNotExist)
            }
            Response::Error { error } => Err(anyhow!("{error}")),
            Response::Page(collection_page) => Ok(CollectionResponse::Public {
                collection: collection_page,
            }),
        }
    }

    pub async fn get_card(&self, card_id: &str) -> Result<Card> {
        let client = reqwest::Client::new();
        let url = reqwest::Url::parse_with_params(
            &supabase_url!("cards"),
            &[("select", "*".to_string()), ("id", format!("eq.{card_id}"))],
        )?;
        let mut result: Vec<Card> = client
            .get(url)
            .bearer_auth(self.supabase_bearer.read().await.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .send()
            .await?
            .json()
            .await?;
        Ok(result.pop().unwrap())
    }
}
