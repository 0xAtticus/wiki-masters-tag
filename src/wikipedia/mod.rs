use anyhow::Result;
use sqlx::FromRow;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;

pub struct WikipediaRestApi {
    category_link_db: sqlx::SqlitePool,
    page_db: sqlx::SqlitePool,
    link_target_db: sqlx::SqlitePool,
}

type CategoryName = String;

#[allow(dead_code)]
#[derive(Debug, FromRow)]
struct Page {
    page_id: i64,
    page_title: String,
}

#[allow(dead_code)]
#[derive(Debug, FromRow)]
struct CategoryLink {
    cl_from: i64,      // page_id
    cl_target_id: i64, // category title
}

#[allow(dead_code)]
#[derive(Debug, FromRow)]
struct LinkTarget {
    lt_id: i64,        // primary key
    lt_title: String,  // target title (e.g., "Category:Programming_languages")
    lt_namespace: i64, // namespace ID (e.g., 14 for categories)
}

const FORBIDDEN_PREFIX: [&str; 8] = [
    "Projet:",
    "Portail:",
    "Wikipédia:",
    "Catégorie_",
    "Période_historique",
    "Article",
    "Espace_encyclopédique",
    "Chronologie",
];

pub struct CategoryIterator<'a> {
    pile: VecDeque<PageCategory>,
    seen: HashSet<CategoryName>,
    wikipedia_api: &'a WikipediaRestApi,
}

impl<'a> CategoryIterator<'a> {
    pub async fn new(wikipedia_api: &'a WikipediaRestApi, base_page: &str) -> CategoryIterator<'a> {
        let base_parents: Vec<String> = wikipedia_api.get_parent(base_page, true).await.unwrap();
        CategoryIterator {
            pile: base_parents
                .into_iter()
                .map(|parent| PageCategory {
                    category_name: parent,
                    path_to_category: vec![base_page.to_owned()],
                })
                .collect(),
            wikipedia_api,
            seen: HashSet::new(),
        }
    }

    pub async fn next(&mut self) -> Result<Option<PageCategory>> {
        while let Some(page_category) = self.pile.pop_front() {
            if self.seen.contains(&page_category.category_name) {
                continue; // We already had a path for this category, and it was shorter, so do not return 
            }
            if FORBIDDEN_PREFIX
                .iter()
                .any(|prefix| page_category.category_name.starts_with(prefix))
            {
                continue;
            }
            self.seen.insert(page_category.category_name.clone());
            let parents = self
                .wikipedia_api
                .get_parent(&page_category.category_name, false)
                .await?;
            for parent in parents {
                let mut old_path = page_category.path_to_category.clone();
                old_path.push(page_category.category_name.clone());
                self.pile.push_back(PageCategory {
                    category_name: parent,
                    path_to_category: old_path,
                });
            }
            return Ok(Some(page_category));
        }
        Ok(None)
    }
}

pub struct PageCategory {
    pub category_name: String,
    pub path_to_category: Vec<String>,
}

pub const CATEGORY_LINK_FILE: &str = "categorylink.db";
pub const PAGE_FILE: &str = "page.db";
pub const LINK_TARGET_FILE: &str = "linktarget.db";

impl WikipediaRestApi {
    pub async fn new(database_folder: &Path) -> Self {
        Self {
            category_link_db: sqlx::SqlitePool::connect(
                database_folder.join(CATEGORY_LINK_FILE).to_str().unwrap(),
            )
            .await
            .unwrap(),
            page_db: sqlx::SqlitePool::connect(database_folder.join(PAGE_FILE).to_str().unwrap())
                .await
                .unwrap(),
            link_target_db: sqlx::SqlitePool::connect(
                database_folder.join(LINK_TARGET_FILE).to_str().unwrap(),
            )
            .await
            .unwrap(),
        }
    }

    async fn get_parent(&self, page_title: &str, is_first_page: bool) -> Result<Vec<CategoryName>> {
        let namespace = if is_first_page { 0 } else { 14 }; // 14 is for categories
        tracing::debug!("Fetching parent for {page_title}, {is_first_page}");
        let page: Option<Page> = sqlx::query_as("SELECT page_id,  CAST(page_title AS TEXT) as page_title FROM page WHERE page_title = ? AND page_namespace = ?")
                            .bind(page_title)
                            .bind(namespace)
                            .fetch_optional(&self.page_db)
                            .await?;
        let page = match page {
            // Some weird page such as Politique_culturelle_en_Ukraine do not really exists, but still have children. Let's ignore them
            None => {
                return Ok(Vec::new());
            }
            Some(page) => page,
        };
        // Step 2: Query all categorylinks for the page_id
        let category_links: Vec<CategoryLink> =
            sqlx::query_as("SELECT cl_from, cl_target_id FROM categorylinks WHERE cl_from = ?")
                .bind(page.page_id)
                .fetch_all(&self.category_link_db)
                .await?;

        // Step 3: Resolve cl_target_id to category titles using the linktarget table
        let category_ids: Vec<i64> = category_links
            .into_iter()
            .map(|cl| cl.cl_target_id)
            .collect();

        // Step 4: Query the linktarget table for the titles
        let placeholders = vec!["?"; category_ids.len()].join(",");
        let query = format!(
            "SELECT lt_id, CAST(lt_title as text) as lt_title, lt_namespace FROM linktarget WHERE lt_id IN ({}) AND lt_namespace = 14",
            placeholders
        );

        let mut query = sqlx::query_as::<_, LinkTarget>(&query);
        for id in category_ids {
            query = query.bind(id);
        }
        let link_targets: Vec<LinkTarget> = query.fetch_all(&self.link_target_db).await?;
        Ok(link_targets.into_iter().map(|link| link.lt_title).collect())
    }
}
