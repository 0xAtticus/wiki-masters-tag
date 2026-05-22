mod api;
mod migrate_dump;
mod wikipedia;

use std::{path::PathBuf, str::FromStr, sync::Arc};

use crate::{
    api::{
        AttachTagRequest, Card, CardRarity, CollectedCard, CreateTagRequest,
        DeleteTagFromCardRequest, DeleteTagRequest, SetTagColorRequest, Tag, User,
    },
    migrate_dump::{CategoryLinkMigrator, LinkTargetMigrator, Migrator, PageMigrator},
    wikipedia::{CategoryIterator, WikipediaRestApi},
};
use anyhow::Result;
use api::WikiMasterApi;
use clap::{Parser, Subcommand};
use hex_color::HexColor;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rand::seq::IndexedRandom;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::{self, io::AsyncWriteExt};

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
