mod api;
mod local_state;
mod migrate_dump;
mod wikipedia;

use std::{
    collections::HashSet, path::PathBuf, str::FromStr, sync::Arc, time::Duration,
};

use crate::{
    api::{
        AttachTagRequest, Card, CardRarity, CollectedCard, CollectionResponse, CreateTagRequest,
        DeleteTagFromCardRequest, DeleteTagRequest, SetTagColorRequest, Tag, User,
    },
    local_state::{GetUserResult, LocalState, LocalUser},
    migrate_dump::{CategoryLinkMigrator, LinkTargetMigrator, Migrator, PageMigrator},
    wikipedia::{CategoryIterator, WikipediaRestApi},
};
use anyhow::Result;
use api::WikiMasterApi;
use clap::{Parser, Subcommand};
use hex_color::HexColor;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use itertools::Itertools;
use rand::seq::IndexedRandom;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tokio::{self, io::AsyncWriteExt, sync::RwLock, sync::Semaphore};

const SUPABASE_API_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImN5cnhqZXBwanFzeHhqYXlmcnVyIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NzM4ODAzMzksImV4cCI6MjA4OTQ1NjMzOX0.BZluyXygNxuQGDPxFX1zG5i-cqp10CVK-8GGtuak4Rg";

#[derive(Debug, Parser)]
#[clap(name = "wiki-master-tag", version)]
pub struct App {
    #[arg(short, long, default_value = "config.yml")]
    config_file: String,
    #[arg(long, default_value = "./wikipedia_database")]
    database_folder_path: PathBuf,
    #[arg(long)]
    email: String,
    #[arg(long)]
    password: String,
    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Allows to retag all your collection according to the newest config file. This will remove all previous tags.
    RetagAll,
    /// Retag only newest untagged cards according to the config file.
    TagNew,
    /// Allow you to debug why a given card was tagged with a specific tag.
    DryRun {
        #[clap(long, short = 't')]
        page_title: String,
    },
    /// Needed to be run once to download the wikipedia database.
    Init,
    /// Send all the cards with the given tag to a given user.
    Trade {
        #[clap(long, short = 'e')]
        etiquette: String,
        #[clap(long, short = 'u')]
        user: String,
    },
    /// Allow you to sell random cards according to defined prices.
    Sell {
        #[clap(long, default_value_t = 1500)]
        l_price: usize,
        #[clap(long, default_value_t = 300)]
        ur_price: usize,
        #[clap(long, default_value_t = 300)]
        sr_price: usize,
        #[clap(long, default_value_t = 100)]
        r_price: usize,
        #[clap(long, default_value_t = 50)]
        pc_price: usize,
        #[clap(long, default_value_t = 50)]
        c_price: usize,
        #[clap(long, short = 'd', default_value_t = 10)]
        duration_minutes: usize,
        /// Useful parameter in you want to avoid selling low rarity cards that will not sell, and you want to keep to increase your card number.
        /// Inclusive.
        #[clap(long, default_value_t = CardRarity::R)]
        #[arg(value_enum)]
        minimum_rarity: CardRarity,
    },
    /// Search for the owner of a card. The list of user is obtained using the db create by `Command::FetchAllUsers`.
    Search {
        #[clap(long)]
        card_id: String,
    },
    // Download all users into a local sqlite database.
    FetchAllUsers,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = App::parse();
    let config: Config = {
        use std::io::Write;
        let file = match std::fs::File::open(&args.config_file) {
            Ok(f) => f,
            Err(_) => {
                let mut f = std::fs::File::create_new(&args.config_file)?;
                f.write_all(serde_yaml::to_string(&Config::default())?.as_bytes())?;
                f.flush()?;
                std::fs::File::open(&args.config_file)?
            }
        };
        serde_yaml::from_reader(file)?
    };

    let local_state = Arc::new(local_state::LocalState::new().await);

    tracing_subscriber::fmt::init();

    if let Command::Init = args.command {
        std::fs::create_dir_all(&args.database_folder_path)?;
        let database_folder_path = Arc::new(args.database_folder_path);
        let multi_progress_bar = Arc::new(MultiProgress::new());

        let database_folder_path_local = database_folder_path.clone();
        let multi_progress_bar_local = multi_progress_bar.clone();
        let page_handle = tokio::spawn(async move {
            let file_path = database_folder_path_local
                .clone()
                .join(wikipedia::PAGE_FILE);
            download_and_migrate_db(
                Url::from_str(
                    "https://dumps.wikimedia.org/frwiki/latest/frwiki-latest-page.sql.gz",
                )
                .unwrap(),
                &file_path,
                PageMigrator,
                &multi_progress_bar_local,
            )
            .await
        });

        let database_folder_path_local = database_folder_path.clone();
        let multi_progress_bar_local = multi_progress_bar.clone();
        let categorylink_handle = tokio::spawn(async move {
            download_and_migrate_db(
                Url::from_str(
                    "https://dumps.wikimedia.org/frwiki/latest/frwiki-latest-categorylinks.sql.gz",
                )
                .unwrap(),
                &database_folder_path_local.join(wikipedia::CATEGORY_LINK_FILE),
                CategoryLinkMigrator,
                &multi_progress_bar_local,
            )
            .await
        });

        let database_folder_path_local = database_folder_path.clone();
        let multi_progress_bar_local = multi_progress_bar.clone();
        let linktarget_handle = tokio::spawn(async move {
            download_and_migrate_db(
                Url::from_str(
                    "https://dumps.wikimedia.org/frwiki/latest/frwiki-latest-linktarget.sql.gz",
                )
                .unwrap(),
                &database_folder_path_local.join(wikipedia::LINK_TARGET_FILE),
                LinkTargetMigrator,
                &multi_progress_bar_local,
            )
            .await
        });

        page_handle.await??;
        categorylink_handle.await??;
        linktarget_handle.await??;

        return Ok(());
    }

    let api =
        WikiMasterApi::from_credentials(&args.email, &args.password, SUPABASE_API_KEY.to_string())
            .await?;
    let wikipedia = WikipediaRestApi::new(&args.database_folder_path).await;
    let wikipedia = Arc::new(wikipedia);
    let api = Arc::new(api);
    let etiquettes = Arc::new(config.etiquettes);
    match args.command {
        Command::RetagAll => {
            let pb = ProgressBar::no_length();
            pb.set_style(ProgressStyle::default_bar()
                .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({per_sec}, {eta})")?);
            delete_all_tags(api.clone(), &pb).await?;
            let mut new_tags = vec![];
            pb.set_length(etiquettes.len() as u64);
            pb.set_position(0);
            pb.set_message("Creating tags.");
            for etiquette in etiquettes.iter() {
                api.create_tag(CreateTagRequest {
                    name: &etiquette.name,
                })
                .await?;
                pb.inc(1);
                new_tags.push(etiquette.name.clone());
            }

            let tags = api.list_tags().await?;

            for tag in new_tags {
                api.set_tag_color(SetTagColorRequest {
                    color: HexColor::random_rgb(),
                    tag_id: tags.iter().find(|t| t.name == tag).unwrap().id.clone(),
                })
                .await?;
            }

            let tags = Arc::new(tags);
            let mut page: usize = 0;
            pb.set_message("Tagging cards");
            pb.set_position(0);
            loop {
                let collection = api.my_collection(page, None).await?;
                if let Some(total) = collection.total {
                    pb.set_length(total as u64);
                }
                let cards_count = collection.collection.len();
                let mut card_futures = vec![];
                for card in collection.collection {
                    let api = api.clone();
                    let wikipedia = wikipedia.clone();
                    let tags = tags.clone();
                    let etiquettes = etiquettes.clone();
                    card_futures.push(tokio::spawn(async move {
                        tag_card(&wikipedia, &api, &card, &tags, &etiquettes, false).await
                    }));
                }
                for f in card_futures {
                    f.await??;
                }
                pb.inc(cards_count as u64);
                if cards_count != 50 {
                    break;
                }
                page += 1;
            }
        }
        Command::TagNew => {
            tracing::info!("Fetching tags");
            let existing_tags = api.list_tags().await?;

            tracing::info!("Creating new tags.");
            let mut new_tags = vec![];
            for etiquette in etiquettes
                .iter()
                .filter(|e| !existing_tags.iter().any(|t| t.name == e.name))
            {
                api.create_tag(CreateTagRequest {
                    name: &etiquette.name,
                })
                .await?;
                new_tags.push(etiquette.name.clone());
            }

            tracing::info!("Refreshing tags.");
            let tags = api.list_tags().await?;

            for tag in new_tags {
                api.set_tag_color(SetTagColorRequest {
                    color: HexColor::random_rgb(),
                    tag_id: tags.iter().find(|t| t.name == tag).unwrap().id.clone(),
                })
                .await?;
            }

            tracing::info!("Tags synced.");
            let mut page: usize = 0;

            let tags = Arc::new(tags);
            loop {
                tracing::info!("Processing page {page}");
                let collection = api.my_collection(page, None).await?;
                let untagged_cards: Vec<_> = collection
                    .collection
                    .into_iter()
                    .filter(|card| card.tags.is_empty())
                    .collect();
                let cards_count = untagged_cards.len();
                let mut card_futures = vec![];
                for card in untagged_cards {
                    let api = api.clone();
                    let wikipedia = wikipedia.clone();
                    let tags = tags.clone();
                    let etiquettes = etiquettes.clone();
                    card_futures.push(tokio::spawn(async move {
                        tag_card(&wikipedia, &api, &card, &tags, &etiquettes, false).await
                    }));
                }
                for f in card_futures {
                    f.await??;
                }
                if cards_count != 50 {
                    break;
                }
                page += 1;
            }
        }
        Command::DryRun { page_title } => {
            let tags = api.list_tags().await?;
            tag_card(
                &wikipedia,
                &api,
                &CollectedCard {
                    id: "fake".to_string(),
                    tags: vec![],
                    card: Card {
                        id: "fake".to_string(),
                        wikipedia_url: "fake".to_string(),
                        wikipedia_title: page_title,
                        rarity: api::CardRarity::C,
                    },
                },
                &tags,
                &etiquettes,
                false,
            )
            .await?;
        }
        Command::Trade { etiquette, user } => {
            let tags = api.list_tags().await?;
            let etiquette = tags
                .into_iter()
                .find(|t| t.name == etiquette)
                .expect("Le tag n'est pas valide.");

            let user: User = api
                .friends()
                .await?
                .into_iter()
                .find(|friend| friend.name == user)
                .expect("Could not find friend");
            let mut all_cards = vec![];
            let mut page = 0;
            let is_tradeable = |card: &CollectedCard| {
                !card.tags.iter().any(|tag| {
                    etiquettes
                        .iter()
                        .any(|e| e.name == tag.name && !e.allow_trade)
                })
            };
            let pb = ProgressBar::no_length();
            pb.set_style(ProgressStyle::default_bar()
                .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({per_sec}, {eta})")?);
            loop {
                let collection = api.my_collection(page, Some(&etiquette.id)).await?;

                if let Some(total) = collection.total {
                    pb.set_length(total as u64);
                }
                let cards_count = collection.collection.len();
                all_cards.extend(collection.collection.into_iter().filter(is_tradeable));
                if cards_count != 50 {
                    break;
                }
                pb.inc(cards_count as u64);
                page += 1;
            }

            tracing::info!("Found {} cards to trade", all_cards.len());
            for chunk in all_cards.chunks(100) {
                api.create_trade(
                    &chunk
                        .iter()
                        .map(|card| card.card.id.as_str())
                        .collect::<Vec<_>>(),
                    &user.id,
                )
                .await?;
            }
        }
        Command::Sell {
            duration_minutes,
            l_price,
            ur_price,
            sr_price,
            r_price,
            pc_price,
            c_price,
            minimum_rarity,
        } => {
            let mut all_cards = vec![];
            let mut page = 0;
            let is_sellable = |card: &CollectedCard| {
                !card.tags.iter().any(|tag| {
                    etiquettes
                        .iter()
                        .any(|e| e.name == tag.name && !e.allow_sell)
                })
            };
            let pb = ProgressBar::no_length();
            pb.set_message("Fetching all cards");
            pb.set_style(ProgressStyle::default_bar()
                .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({per_sec}, {eta})")?);
            loop {
                // TODO: Could be faster by requesting collection ordered by rarity and stopping when we reach quality < minimum_rarity`.`
                let collection = api.my_collection(page, None).await?;

                if let Some(total) = collection.total {
                    pb.set_length(total as u64);
                }
                let cards_count = collection.collection.len();
                all_cards.extend(
                    collection
                        .collection
                        .into_iter()
                        .filter(is_sellable)
                        .filter(|c| c.card.rarity <= minimum_rarity),
                );
                if cards_count != 50 {
                    break;
                }
                pb.inc(cards_count as u64);
                page += 1;
            }

            pb.reset();
            pb.set_message("Selling cards");
            let n_cards_to_sell = 5;
            pb.set_length(n_cards_to_sell as u64);
            let to_sell = all_cards.sample(&mut rand::rng(), n_cards_to_sell);
            for card in to_sell {
                let price = match card.card.rarity {
                    api::CardRarity::L => l_price,
                    api::CardRarity::UR => ur_price,
                    api::CardRarity::SR => sr_price,
                    api::CardRarity::R => r_price,
                    api::CardRarity::PC => pc_price,
                    api::CardRarity::C => c_price,
                };
                api.sell_card(&card.card.id, duration_minutes, price)
                    .await?;
                pb.inc(1);
            }
        }
        Command::Search { card_id } => {
            let card = Arc::new(api.get_card(&card_id).await?);
            let users = local_state.users.list_users().await?;
            let pb = ProgressBar::new(users.len() as u64);
            pb.set_message("Fetching all cards");
            pb.set_style(ProgressStyle::default_bar()
                .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({per_sec}, {eta})")?);
            let pb = Arc::new(pb);
            let owners = Arc::new(Mutex::new(vec![]));
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<User>();

            let writer_handle = tokio::spawn(async move {
                for user in users {
                    let forbidden_chars = ['/', '#', '?'];
                    if forbidden_chars.iter().any(|c| user.name.contains(*c)) {
                        // api is bugged
                        continue;
                    }
                    tx.send(user).unwrap();
                }
            });
            let semaphore: Arc<Semaphore> = Arc::new(Semaphore::new(128));
            let mut handles: Vec<tokio::task::JoinHandle<anyhow::Result<_>>> = Vec::new();

            while let Some(user) = rx.recv().await {
                let api = api.clone();
                let owners = owners.clone();
                let pb = pb.clone();
                let semaphore = semaphore.clone();
                let card = card.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = semaphore.acquire().await.unwrap();
                    let mut page = 0;
                    loop {
                        // The app is too slow and timeouts if we actually search for the card, so we have to manually look at all the pages.
                        let mut failure_count = 0;
                        let collection = loop {
                            let r = api.user_collection(&user.name, page, None).await;
                            match r {
                                Ok(page) => {
                                    break page;
                                }
                                Err(e) => {
                                    failure_count += 1;
                                    if failure_count > 6 {
                                        tracing::info!(
                                            "Get collection: {} failures for {} page {}: {e}",
                                            failure_count,
                                            user.name,
                                            page
                                        );
                                    }
                                    tokio::time::sleep(Duration::from_secs(10)).await;
                                }
                            }
                        };
                        page += 1;
                        if let CollectionResponse::Public { collection } = collection {
                            if collection
                                .collection
                                .iter()
                                .any(|collected_card| collected_card.card.id == card.id)
                            {
                                pb.set_message(format!("User {} has card !", user.name));
                                owners.lock().unwrap().push(user.name);
                                break;
                            }
                            if collection.collection.len() < 50 {
                                break;
                            }
                            if collection.collection.last().unwrap().card.rarity > card.rarity {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    pb.inc(1);
                    Ok(())
                }));
            }
            writer_handle.await?;
            for handle in handles {
                handle.await??;
            }
            pb.finish_and_clear();
            let owners = owners.lock().unwrap().clone();
            tracing::info!("Found the following users with the card: {owners:?}")
        }
        Command::FetchAllUsers => {
            refresh_users_local_state(api, local_state).await?;
        }
        Command::Init => {
            unreachable!("This case is catched before.")
        }
    }

    Ok(())
}

async fn download_and_migrate_db(
    url: Url,
    output_path: &PathBuf,
    migrator: impl Migrator,
    progress: &MultiProgress,
) -> Result<()> {
    use futures::stream::StreamExt;
    let pb = progress.add(ProgressBar::no_length());
    pb.set_style(ProgressStyle::default_bar()
        .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?);
    pb.set_message(format!("Downloading {}", url));

    let response = reqwest::get(url.clone()).await?;
    let file_size = response.content_length().unwrap();
    pb.set_length(file_size);
    let mut bytes = response.bytes_stream();
    let mut buffer = Vec::with_capacity(file_size as usize);
    while let Some(item) = bytes.next().await {
        let chunk = item?;
        buffer.write_all(&chunk).await?;
        pb.inc(chunk.len() as u64);
    }

    let cursor = std::io::Cursor::new(buffer);
    // Decompress the gzip stream on the fly
    let decompressed = flate2::read::GzDecoder::new(cursor);

    let mut reader = std::io::BufReader::new(decompressed);
    std::fs::File::create(output_path)?;
    migrator.migrate(&mut reader, output_path, &pb).await?;
    // Dropping should clean the file ?
    Ok(())
}

async fn refresh_users_local_state(
    api: Arc<WikiMasterApi>,
    local_state: Arc<LocalState>,
) -> Result<usize> {
    let added_users = 0usize;
    let fully_explored = Arc::new(RwLock::new(Vec::new())); // Just another layer of cache over the LocalState for faster responses.
    /// Allow to filter out unused chars in usernames, and greatly speed up the rest of the process.
    async fn get_char_shortlist(api: Arc<WikiMasterApi>) -> Result<Vec<char>> {
        let mut result = HashSet::new();
        let mut handles = Vec::new();

        let semaphore: Arc<Semaphore> = Arc::new(Semaphore::new(64));
        for c in 32..=u8::MAX {
            let c = c as char;
            let semaphore = semaphore.clone();
            let api = api.clone();
            let handle = tokio::spawn(async move {
                let _permit = semaphore.acquire().await.unwrap();
                let users_prefixed = api.search_users(&format!("{c}_")).await.unwrap();
                let users_suffixed = api.search_users(&format!("_{c}")).await.unwrap();
                !(users_prefixed.is_empty() && users_suffixed.is_empty())
            });
            handles.push((c as char, handle));
        }

        for (c, handle) in handles {
            if handle.await? {
                result.insert(c.to_ascii_lowercase());
            }
        }
        Ok(result
            .into_iter()
            .filter(|c| *c != '\\' && *c != '*' && *c != '%')
            .collect())
    }

    async fn search_prefix_inner(api: &WikiMasterApi, prefix: String) -> Result<Vec<User>> {
        tracing::info!("prefix: {prefix}",);

        let mut users = vec![];
        let mut failure_count = 0;
        loop {
            match api.search_users(&prefix).await {
                Err(e) => {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    failure_count += 1;
                    tracing::info!("Search: {} failures for {prefix}: {e}", failure_count);
                }
                Ok(u) => {
                    users = u;
                    break;
                }
            }
        }
        Ok(users)
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut handles = Vec::new();
    tracing::info!("Creating character shorlist");
    let char_shortlist = Arc::new(get_char_shortlist(api.clone()).await?);
    tracing::info!("Got char shortlist: {char_shortlist:?}");
    {
        let char_shortlist = char_shortlist.clone();

        let tx = tx.clone();
        let handle = tokio::spawn(async move {
            for (c1, c2) in char_shortlist
                .iter()
                .cartesian_product(char_shortlist.iter())
            {
                let mut p = String::new();
                if *c1 == '_' {
                    p.push_str("\\_")
                } else {
                    p.push(*c1);
                }
                if *c2 == '_' {
                    p.push_str("\\_")
                } else {
                    p.push(*c2);
                }
                tx.send(p).unwrap();
            }
        });
        handles.push(handle);
    }
    let semaphore: Arc<Semaphore> = Arc::new(Semaphore::new(128));

    let mut handles = vec![];
    while let Some(prefix) = rx.recv().await {
        if &prefix == ".." {
            continue;
        }
        let tx = tx.clone();
        let semaphore = semaphore.clone();
        let char_shortlist = char_shortlist.clone();
        let api = api.clone();
        let local_state = local_state.clone();
        let fully_explored = fully_explored.clone();
        let handle = tokio::spawn(async move {
            if fully_explored
                .read()
                .await
                .iter()
                .any(|fully_explored_prefix| prefix.contains(fully_explored_prefix))
            {
                tracing::info!("Skip {}", prefix);
                return;
            }
            let prefix_count: Option<i64> = local_state.users.get_prefix(&prefix).await.unwrap();
            let users_count = if let Some(prefix_count) = prefix_count {
                if prefix_count < 10 {
                    // Assume that if a prefix is in the cache, we already fetched the users
                    fully_explored.write().await.push(prefix);
                    return;
                }
                prefix_count
            } else {
                let _permit = semaphore.acquire().await.unwrap();

                let users = search_prefix_inner(&api, prefix.clone()).await.unwrap();

                local_state
                    .users
                    .store_prefix(&prefix, users.len() as i64)
                    .await
                    .unwrap();
                users.len() as i64
            };
            // In this case, we need to dive deeper
            if users_count >= 10 {
                // Check if there is a user named like this prefix
                let prefix_underscore = &prefix.replace("\\_", "_");
                match local_state
                    .users
                    .get_user_by_name(&prefix_underscore)
                    .await
                    .unwrap()
                {
                    GetUserResult::CacheMiss => {
                        let _permit = semaphore.acquire().await.unwrap();
                        let mut failure_count = 0;
                        let user = loop {
                            let r = api.get_user(&prefix_underscore).await;
                            match r {
                                Ok(user) => {
                                    break user;
                                }
                                Err(e) => {
                                    failure_count += 1;
                                    tracing::info!(
                                        "Get user: {} failures for {prefix_underscore}: {e}",
                                        failure_count
                                    );
                                    tokio::time::sleep(Duration::from_secs(10)).await;
                                }
                            }
                        };
                        let user_to_store = match user {
                            None => LocalUser {
                                id: None,
                                name: prefix_underscore.clone(),
                            },
                            Some(user) => LocalUser {
                                id: Some(user.id),
                                name: user.name,
                            },
                        };
                        local_state.users.store_user(user_to_store).await.unwrap()
                    }
                    _ => {}
                }
                for c in char_shortlist.iter() {
                    let mut p = prefix.clone();
                    if *c == '_' {
                        p.push_str("\\_")
                    } else {
                        p.push(*c);
                    }
                    tx.send(p).unwrap();
                }
            } else if users_count > 0 {
                let users = {
                    let _permit = semaphore.acquire().await.unwrap();
                    search_prefix_inner(&api, prefix.clone()).await.unwrap()
                };

                for user in users {
                    local_state
                        .users
                        .store_user(LocalUser {
                            id: Some(user.id),
                            name: user.name,
                        })
                        .await
                        .unwrap();
                }
                fully_explored.write().await.push(prefix);
            } else {
                fully_explored.write().await.push(prefix);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    Ok(added_users)
}

async fn delete_all_tags(api: Arc<WikiMasterApi>, pb: &ProgressBar) -> Result<()> {
    // First we need to remove all tags from all cards, otherwise deleting the tag takes too long
    pb.set_message("Deleting old tags");
    let mut page = 0;
    loop {
        let collection = api.my_collection(page, None).await?;
        if let Some(total) = collection.total {
            pb.set_length(total as u64);
        }
        let cards_count = collection.collection.len();
        let mut card_futures = vec![];
        for card in collection.collection {
            let api = api.clone();
            card_futures.push(tokio::spawn(async move {
                api.delete_tag_from_card(DeleteTagFromCardRequest {
                    user_card_id: &card.id,
                })
                .await
            }));
        }
        for f in card_futures {
            f.await??;
        }
        pb.inc(cards_count as u64);
        if cards_count != 50 {
            break;
        }
        page += 1;
    }
    // Then we can delete the tags
    let tags = api.list_tags().await?;
    let mut delete_handle = Vec::new();
    for tag in tags {
        let api = api.clone();
        delete_handle.push(tokio::spawn(async move {
            api.delete_tag(DeleteTagRequest { tag_id: &tag.id }).await
        }));
    }
    for f in delete_handle {
        f.await??;
    }
    Ok(())
}

#[derive(Debug)]
struct EtiquetteClassification<'a> {
    etiquette: &'a Etiquette,
    category: String,
    path_to_category: Vec<String>,
}

async fn get_etiquettes_to_apply<'a>(
    wikipedia_api: &WikipediaRestApi,
    etiquettes: &'a [Etiquette],
    card: &CollectedCard,
) -> Result<Vec<EtiquetteClassification<'a>>> {
    let mut result = vec![];
    tracing::debug!("Getting etiquettes for {:?}", card);
    let mut category_iterator =
        CategoryIterator::new(wikipedia_api, &card.card.wikipedia_title.replace(" ", "_")).await;
    let mut best_path_length = usize::MAX;
    while let Some(category) = category_iterator.next().await? {
        for etiquette in etiquettes {
            if etiquette
                .wikipedia_categories
                .iter()
                .any(|wikipedia_category| {
                    wikipedia_category.name == category.category_name
                        && wikipedia_category.max_path_length >= category.path_to_category.len()
                        && !category.path_to_category.iter().any(|category_in_path| {
                            wikipedia_category
                                .forbidden_category_in_path
                                .contains(category_in_path)
                        })
                })
            {
                if best_path_length == usize::MAX {
                    best_path_length = category.path_to_category.len();
                }
                if category.path_to_category.len() <= best_path_length {
                    result.push(EtiquetteClassification {
                        etiquette,
                        category: category.category_name.clone(),
                        path_to_category: category.path_to_category.clone(),
                    });
                } else {
                    return Ok(result);
                }
            }
        }
    }
    Ok(result)
}

async fn tag_card(
    wikipedia_api: &WikipediaRestApi,
    wikimaster: &WikiMasterApi,
    card: &CollectedCard,
    tags: &[Tag],
    etiquettes: &[Etiquette],
    dry_run: bool,
) -> Result<()> {
    let etiquettes = get_etiquettes_to_apply(wikipedia_api, etiquettes, card).await?;

    for etiquette in etiquettes.iter() {
        let tag_id = tags
            .iter()
            .find(|tag| tag.name == etiquette.etiquette.name)
            .map(|tag| tag.id.clone())
            .unwrap(); // It should have been created so unwrap should be fine.

        tracing::info!(
            "Attaching etiquette {} to {} because of category: {} reached with {:?}",
            etiquette.etiquette.name,
            card.card.wikipedia_title,
            etiquette.category,
            etiquette.path_to_category
        );
        if !dry_run {
            wikimaster
                .attach_tag(AttachTagRequest {
                    tag_id,
                    user_card_id: &card.id,
                })
                .await?;
        }
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct Config {
    etiquettes: Vec<Etiquette>,
}

#[derive(Deserialize, Serialize, Debug)]
struct Etiquette {
    name: String,
    wikipedia_categories: Vec<WikipediaCategory>,
    allow_trade: bool,
    allow_sell: bool,
}

fn default_max_path_length() -> usize {
    100
}

#[derive(Deserialize, Serialize, Debug)]
struct WikipediaCategory {
    name: String,
    #[serde(default = "default_max_path_length")]
    max_path_length: usize,
    #[serde(default)]
    forbidden_category_in_path: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            etiquettes: vec![
                Etiquette {
                    name: "Mes trucs du Japon".to_string(),
                    wikipedia_categories: vec![
                        WikipediaCategory {
                            name: "Histoire_du_Japon".to_string(),
                            max_path_length: 100,
                            forbidden_category_in_path: vec![],
                        },
                        WikipediaCategory {
                            name: "Géographie_du_Japon".to_string(),
                            max_path_length: 100,
                            forbidden_category_in_path: vec![],
                        },
                    ],
                    allow_sell: false,
                    allow_trade: false,
                },
                Etiquette {
                    name: "La cuisine".to_string(),
                    wikipedia_categories: vec![
                        WikipediaCategory {
                            name: "Préparation_culinaire".to_string(),
                            max_path_length: 100,
                            forbidden_category_in_path: vec![],
                        },
                        WikipediaCategory {
                            name: "Cuisinier".to_string(),
                            max_path_length: 100,
                            forbidden_category_in_path: vec![],
                        },
                    ],
                    allow_trade: true,
                    allow_sell: false,
                },
            ],
        }
    }
}
