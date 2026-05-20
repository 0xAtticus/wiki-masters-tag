mod api;
mod migrate_dump;
mod wikipedia;

use std::{path::PathBuf, str::FromStr, sync::Arc};

use crate::{
    api::{
        AttachTagRequest, Card, CollectedCard, CreateTagRequest, DeleteTagFromCardRequest,
        DeleteTagRequest, SetTagColorRequest, Tag, User,
    },
    migrate_dump::{CategoryLinkMigrator, LinkTargetMigrator, Migrator, PageMigrator},
    wikipedia::{CategoryIterator, WikipediaRestApi},
};
use anyhow::Result;
use api::WikiMasterApi;
use clap::{Parser, Subcommand};
use hex_color::HexColor;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use reqwest::Url;
use serde::Deserialize;
use tokio::{self, io::AsyncWriteExt};
use tracing_subscriber::FmtSubscriber;

const SUPABASE_API_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImN5cnhqZXBwanFzeHhqYXlmcnVyIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NzM4ODAzMzksImV4cCI6MjA4OTQ1NjMzOX0.BZluyXygNxuQGDPxFX1zG5i-cqp10CVK-8GGtuak4Rg";

/// Here's my app!
#[derive(Debug, Parser)]
#[clap(name = "my-app", version)]
pub struct App {
    #[arg(short, long, default_value = "config.yml")]
    config_file: String,
    #[arg(long, default_value = "INFO")]
    log_level: String, // TODO ENUM
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
    RetagAll,
    TagNew,
    DryRun {
        #[clap(long, short = 't')]
        page_title: String,
    },
    Init,
    Trade {
        #[clap(long, short = 'e')]
        etiquette: String,
        #[clap(long, short = 'u')]
        user: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = App::parse();
    let config: Config = {
        let f = std::fs::File::open(&args.config_file)?;
        serde_yaml::from_reader(f)?
    };

    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::from_str(&args.log_level).unwrap())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

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
            loop {
                tracing::info!("Processing page {page}");
                let collection = api.my_collection(page, Some(&etiquette.id)).await?;
                let cards_count = collection.collection.len();
                all_cards.extend(collection.collection.into_iter());
                if cards_count != 50 {
                    break;
                }
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

    let reader = std::io::BufReader::new(decompressed);
    std::fs::File::create(output_path)?;
    migrator.migrate(reader, output_path, &pb).await?;
    // Dropping should clean the file ?
    Ok(())
}

async fn delete_all_tags(api: Arc<WikiMasterApi>, pb: &ProgressBar) -> Result<()> {
    // First we need to remove all tags from all cards, otherwise deleting the tag takes too long
    pb.set_message("Deleting old tags");
    let mut page = 0;
    loop {
        tracing::info!("Processing page {page}");
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
                .contains(&category.category_name)
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

#[derive(Deserialize)]
struct Config {
    etiquettes: Vec<Etiquette>,
}

#[derive(Deserialize, Debug)]
struct Etiquette {
    name: String,
    wikipedia_categories: Vec<String>,
}
