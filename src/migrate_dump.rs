use anyhow::Result;
use anyhow::anyhow;
use indicatif::{ProgressBar, ProgressStyle};
use nom::{
    IResult, Parser,
    branch::alt,
    bytes::complete::{tag, take_while1},
    character::complete::{anychar, char, digit1},
    combinator::{map, map_res, recognize},
    multi::{many0, separated_list0},
    sequence::{delimited, preceded, separated_pair, terminated},
};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use std::io::BufRead;
use std::path::Path;

#[derive(Debug, PartialEq)]
enum Field {
    Integer(i64),
    Float(f32),
    String(String),
    Null,
}
#[derive(Debug, PartialEq)]
struct Record {
    fields: Vec<Field>,
}

// Helper function to parse a string with escaped quotes
fn parse_string(input: &str) -> IResult<&str, String> {
    delimited(
        char('\''),
        map(
            many0(alt((
                take_while1(|c: char| c != '\'' && c != '\\'),
                recognize(preceded(char('\\'), anychar)),
            ))),
            |s: Vec<&str>| {
                s.into_iter()
                    .map(|s| s.strip_prefix("\\").unwrap_or(s))
                    .collect()
            },
        ),
        char('\''),
    )
    .parse(input)
}

fn parse_float(input: &str) -> IResult<&str, f32> {
    map(separated_pair(digit1, char('.'), digit1), |_| 0f32).parse(input) // We actually do not care about the value of any float field, we just need to parse it.
}

fn parse_number(input: &str) -> IResult<&str, i64> {
    alt((
        map_res(digit1, |s: &str| s.parse::<i64>()),
        map_res(preceded(char('-'), digit1), |s: &str| {
            s.parse::<i64>().map(|d| -d)
        }),
    ))
    .parse(input)
}

fn parse_field(input: &str) -> IResult<&str, Field> {
    // if input.len() > 20 {
    //     let mut low_bound = 19;
    //     while !input.is_char_boundary(low_bound) {
    //         low_bound -= 1;
    //     }
    //     println!("field: {}...", &input[..low_bound]);
    // }
    alt((
        map(tag("NULL"), |_| Field::Null),
        map(parse_float, Field::Float),
        map(parse_number, Field::Integer),
        map(parse_string, Field::String),
    ))
    .parse(input)
}

fn parse_record(input: &str) -> IResult<&str, Record> {
    map(
        delimited(
            char('('),
            separated_list0(char(','), parse_field),
            char(')'),
        ),
        |fields| Record { fields },
    )
    .parse(input)
}

fn parse_input(input: &str) -> IResult<&str, Vec<Record>> {
    terminated(separated_list0(char(','), parse_record), char(';')).parse(input)
}

struct LineIterator<'a> {
    dump: &'a mut (dyn BufRead + Send),
    internal_buffer: Vec<u8>,
}

impl<'a> LineIterator<'a> {
    fn new(dump: &'a mut (dyn BufRead + Send)) -> Self {
        Self {
            dump,
            internal_buffer: Vec::new(),
        }
    }
}

impl<'a> Iterator for LineIterator<'a> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        if self
            .dump
            .read_until(b'\n', &mut self.internal_buffer)
            .unwrap()
            > 0
        {
            let line = String::from_utf8_lossy(&self.internal_buffer).into_owned();
            self.internal_buffer.clear();
            Some(line)
        } else {
            None
        }
    }
}

pub struct LinkTargetMigrator;

impl Migrator for LinkTargetMigrator {
    async fn migrate(
        &self,
        dump: &mut (dyn BufRead + Send),
        output: &Path,
        pb: &ProgressBar,
    ) -> Result<()> {
        let connect_opts = output
            .to_str()
            .unwrap()
            .parse::<SqliteConnectOptions>()?
            .journal_mode(SqliteJournalMode::Off)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Off);
        let sqlite = sqlx::SqlitePool::connect_with(connect_opts).await.unwrap();
        sqlx::query("DROP TABLE IF EXISTS `linktarget`;")
            .execute(&sqlite)
            .await?;
        sqlx::query(
            "CREATE TABLE `linktarget` (
  `lt_id` integer  NOT NULL,  
  `lt_namespace` integer NOT NULL,
  `lt_title` varbinary(255) NOT NULL,
  PRIMARY KEY (lt_id)
);
",
        )
        .execute(&sqlite)
        .await?;

        #[derive(Debug)]
        struct Row {
            lt_id: i64,
            lt_namespace: i64,
            lt_title: String,
        }

        fn record_to_row(record: Record) -> Row {
            Row {
                lt_id: match record.fields[0] {
                    Field::Integer(v) => v,
                    _ => unimplemented!(),
                },
                lt_namespace: match record.fields[1] {
                    Field::Integer(v) => v,
                    _ => unimplemented!(),
                },
                lt_title: match &record.fields[2] {
                    Field::String(v) => v.clone(),
                    _ => unimplemented!(),
                },
            }
        }

        fn extract_entries(stmt: &str) -> Result<Vec<Row>> {
            let (_, records) = parse_input(stmt).map_err(|_| anyhow!("ouloulou"))?;
            Ok(records.into_iter().map(record_to_row).collect())
        }

        let mut tx = sqlite.begin().await?;
        let prepared_insert_statement = "INSERT INTO linktarget VALUES(?, ?, ?)";
        pb.unset_length();
        pb.set_style(ProgressStyle::default_bar()
        .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({per_sec}, {eta})")?);
        pb.set_position(0);
        pb.set_message("Migrating linktarget db");

        let mut line_acc = String::new(); // For some reason in this file there is an insert on two lines, so we need to concat them.
        for line in LineIterator::new(dump) {
            line_acc.push_str(line.trim());
            let prefix = "INSERT INTO `linktarget` VALUES ";
            if !line_acc.ends_with(";") {
                continue;
            }
            if !line_acc.starts_with(prefix) {
                line_acc.clear();
                continue;
            }
            let entries = extract_entries(line_acc.strip_prefix(prefix).unwrap())?;
            line_acc.clear();
            for entry in entries {
                pb.inc(1);
                sqlx::query(prepared_insert_statement)
                    .bind(entry.lt_id)
                    .bind(entry.lt_namespace)
                    .bind(entry.lt_title.clone())
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;

        sqlx::query("CREATE INDEX lt_id_idx ON `linktarget`(lt_id);")
            .execute(&sqlite)
            .await?;
        pb.finish_with_message("LinkTarget migration complete.");
        Ok(())
    }
}

pub struct CategoryLinkMigrator;

impl Migrator for CategoryLinkMigrator {
    async fn migrate(
        &self,
        dump: &mut (dyn BufRead + Send),
        output: &Path,
        pb: &ProgressBar,
    ) -> Result<()> {
        let connect_opts = output
            .to_str()
            .unwrap()
            .parse::<SqliteConnectOptions>()?
            .journal_mode(SqliteJournalMode::Off)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Off);
        let sqlite = sqlx::SqlitePool::connect_with(connect_opts).await.unwrap();
        sqlx::query("DROP TABLE IF EXISTS `categorylinks`;")
            .execute(&sqlite)
            .await?;
        sqlx::query(
            "CREATE TABLE `categorylinks` (
  `cl_from` int(8) NOT NULL DEFAULT 0,
  `cl_type` varbinary(255) NOT NULL DEFAULT 'page',
  `cl_target_id` bigint(20) NOT NULL,
  PRIMARY KEY (`cl_from`,`cl_target_id`)
);",
        )
        .execute(&sqlite)
        .await?;

        #[derive(Debug)]
        struct Row {
            cl_from: i64,
            cl_type: String,
            cl_target_id: i64,
        }

        fn record_to_row(record: Record) -> Row {
            Row {
                cl_from: match record.fields[0] {
                    Field::Integer(v) => v,
                    _ => unimplemented!(),
                },
                cl_type: match &record.fields[4] {
                    Field::String(v) => v.clone(),
                    _ => unimplemented!(),
                },
                cl_target_id: match record.fields[6] {
                    Field::Integer(v) => v,
                    _ => unimplemented!(),
                },
            }
        }

        fn extract_entries(stmt: &str) -> Vec<Row> {
            let prefix = "INSERT INTO `categorylinks` VALUES ";

            if !stmt.starts_with(prefix) {
                return vec![];
            }
            let (_, records) = parse_input(stmt.strip_prefix(prefix).unwrap()).unwrap();
            records.into_iter().map(record_to_row).collect()
        }

        let mut transac = sqlite.begin().await?;
        let prepared_insert_statement = "INSERT INTO categorylinks VALUES(?, ?, ?)";
        pb.unset_length();
        pb.set_position(0);
        pb.set_style(ProgressStyle::default_bar()
        .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({per_sec}, {eta})")?);
        pb.set_message("Migrating category link db");

        for line in LineIterator::new(dump) {
            let entries = extract_entries(line.trim());
            for entry in entries {
                pb.inc(1);
                sqlx::query(prepared_insert_statement)
                    .bind(entry.cl_from)
                    .bind(entry.cl_type.clone())
                    .bind(entry.cl_target_id)
                    .execute(&mut *transac)
                    .await?;
            }
        }

        transac.commit().await?;

        sqlx::query("CREATE INDEX cl_from_idx ON `categorylinks`(cl_from);")
            .execute(&sqlite)
            .await?;
        pb.finish_with_message("CategoryLinks migration complete.");
        Ok(())
    }
}

pub struct PageMigrator;

impl Migrator for PageMigrator {
    async fn migrate(
        &self,
        dump: &mut (dyn BufRead + Send),
        output: &Path,
        pb: &ProgressBar,
    ) -> Result<()> {
        let connect_opts = output
            .to_str()
            .unwrap()
            .parse::<SqliteConnectOptions>()?
            .journal_mode(SqliteJournalMode::Off)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Off);
        let sqlite = sqlx::SqlitePool::connect_with(connect_opts).await.unwrap();
        sqlx::query("DROP TABLE IF EXISTS `page`;")
            .execute(&sqlite)
            .await?;
        sqlx::query(
            "CREATE TABLE `page` (
  `page_id` int(8) NOT NULL,
  `page_namespace` int(11) NOT NULL DEFAULT 0,
  `page_title` varbinary(255) NOT NULL DEFAULT '',
  PRIMARY KEY (`page_id`)
);",
        )
        .execute(&sqlite)
        .await?;

        #[derive(Debug)]
        struct Row {
            page_id: i64,
            page_namespace: i64,
            page_title: String,
        }

        fn record_to_row(record: Record) -> Row {
            Row {
                page_id: match record.fields[0] {
                    Field::Integer(v) => v,
                    _ => unimplemented!(),
                },
                page_namespace: match record.fields[1] {
                    Field::Integer(v) => v,
                    _ => unimplemented!(),
                },
                page_title: match &record.fields[2] {
                    Field::String(v) => v.clone(),
                    _ => unimplemented!(),
                },
            }
        }

        fn extract_entries(stmt: &str) -> Vec<Row> {
            let prefix = "INSERT INTO `page` VALUES ";

            if !stmt.starts_with(prefix) {
                return vec![];
            }
            let (_, records) = parse_input(stmt.strip_prefix(prefix).unwrap()).unwrap();
            records.into_iter().map(record_to_row).collect()
        }

        let mut tx = sqlite.begin().await?;
        let prepared_insert_statement = "INSERT INTO page VALUES(?, ?, ?)";
        pb.unset_length();
        pb.set_position(0);
        pb.set_style(ProgressStyle::default_bar()
        .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({per_sec}, {eta})")?);
        pb.set_message("Migrating page db");
        for line in LineIterator::new(dump) {
            let entries = extract_entries(line.trim());
            for entry in entries {
                pb.inc(1);
                sqlx::query(prepared_insert_statement)
                    .bind(entry.page_id)
                    .bind(entry.page_namespace)
                    .bind(&entry.page_title)
                    .execute(&mut *tx)
                    .await?;
            }
        }

        tx.commit().await?;

        sqlx::query("CREATE INDEX page_title_idx ON `page`(page_title);")
            .execute(&sqlite)
            .await?;
        pb.finish_with_message("Page migration complete.");
        Ok(())
    }
}

pub trait Migrator {
    async fn migrate(
        &self,
        dump: &mut (dyn BufRead + Send),
        output: &Path,
        pb: &ProgressBar,
    ) -> Result<()>;
}
