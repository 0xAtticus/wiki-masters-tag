use anyhow::Result;
use base64::prelude::*;
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
    app_cookie: String,
    supabase_api_key: String,
    supabase_bearer: String,
    user_id: String,
}

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

#[derive(Serialize)]
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

        let (cookie_part1, cookie_part2) = app_cookie_b64.split_at(3173);
        Ok(WikiMasterApi {
            supabase_api_key,
            supabase_bearer: response
                .get("access_token")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            app_cookie: format!(
                "sb-cyrxjeppjqsxxjayfrur-auth-token.0=base64-{cookie_part1}; sb-cyrxjeppjqsxxjayfrur-auth-token.1={cookie_part2}"
            ),
            user_id: response
                .get("user")
                .unwrap()
                .get("id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
        })
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
            .header(header::COOKIE, &self.app_cookie)
            .send()
            .await?;
        Ok(response.json().await?)
    }

    pub async fn attach_tag<'a>(&self, request: AttachTagRequest<'a>) -> Result<()> {
        let client = reqwest::Client::new();
        client
            .post(supabase_url!("user_card_tags"))
            .bearer_auth(self.supabase_bearer.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .json(&request)
            .send()
            .await?;
        Ok(())
    }

    pub async fn create_tag<'a>(&self, request: CreateTagRequest<'a>) -> Result<()> {
        let client = reqwest::Client::new();
        client
            .post(supabase_url!("tags"))
            .bearer_auth(self.supabase_bearer.to_owned())
            .header("apikey", self.supabase_api_key.to_owned())
            .json(&request)
            .send()
            .await?;
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
            .bearer_auth(self.supabase_bearer.to_owned())
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
            .bearer_auth(self.supabase_bearer.to_owned())
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
            .bearer_auth(self.supabase_bearer.to_owned())
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
            .bearer_auth(self.supabase_bearer.to_owned())
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
            .header(header::COOKIE, &self.app_cookie)
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
        client
            .post(api_url!("trades"))
            .header(header::COOKIE, &self.app_cookie)
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
            .await?;
        Ok(())
    }
}
