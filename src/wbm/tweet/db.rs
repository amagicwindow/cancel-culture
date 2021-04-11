use crate::browser::twitter::parser::BrowserTweet;
use crate::util::sqlite::{SQLiteDateTime, SQLiteId};
use futures_locks::RwLock;
use rusqlite::{params, Connection, DropBehavior, OptionalExtension, Transaction};
use std::path::Path;
use thiserror::Error;

const USER_SELECT: &str = "
    SELECT id
        FROM user
        WHERE twitter_id = ? AND screen_name = ? AND name = ?
";
const USER_INSERT: &str = "INSERT INTO user (twitter_id, screen_name, name) VALUES (?, ?, ?)";

const FILE_SELECT: &str = "SELECT id FROM file WHERE digest = ?";
const FILE_INSERT: &str = "INSERT INTO file (digest, primary_twitter_id) VALUES (?, ?)";

const TWEET_SELECT_BY_ID: &str = "
    SELECT parent_twitter_id, ts, user_twitter_id, screen_name, name, content, digest
        FROM tweet
        JOIN tweet_file ON tweet_file.tweet_id = tweet.id
        JOIN file ON file.id = tweet_file.file_id
        JOIN user on user.id = tweet_file.user_id
        WHERE tweet.twitter_id = ?
        ORDER BY LENGTH(content) DESC
        LIMIT 1
";

const TWEET_SELECT_FULL: &str = "
    SELECT id
        FROM tweet
        WHERE twitter_id = ? AND parent_twitter_id = ? AND ts = ? AND user_twitter_id = ? AND content = ?
";

const TWEET_INSERT: &str =
    "INSERT INTO tweet (twitter_id, parent_twitter_id, ts, user_twitter_id, content) VALUES (?, ?, ?, ?, ?)";

const TWEET_FILE_INSERT: &str =
    "INSERT INTO tweet_file (tweet_id, file_id, user_id) VALUES (?, ?, ?)";

const USER_SELECT_ALL: &str = "
    SELECT user.twitter_id, tweet.ts, user.screen_name, user.name
        FROM user
        FROM tweet ON tweet.id = (
            SELECT id FROM tweet WHERE tweet.user_twitter_id = user.twitter_id ORDER BY ts DESC LIMIT 1
        )
";

pub type TweetStoreResult<T> = Result<T, TweetStoreError>;

#[derive(Error, Debug)]
pub enum TweetStoreError {
    #[error("Missing file for TweetStore")]
    FileMissing(#[from] std::io::Error),
    #[error("SQLite error for TweetStore")]
    DbFailure(#[from] rusqlite::Error),
}

#[derive(Debug)]
pub struct UserRecord {
    id: u64,
    last_seen: u64,
    screen_names: Vec<String>,
    names: Vec<String>,
}

#[derive(Clone)]
pub struct TweetStore {
    connection: RwLock<Connection>,
}

impl TweetStore {
    pub fn new<P: AsRef<Path>>(path: P, recreate: bool) -> TweetStoreResult<TweetStore> {
        let exists = path.as_ref().is_file();
        let mut connection = Connection::open(path)?;

        if exists {
            if recreate {
                let tx = connection.transaction()?;
                tx.execute("DROP TABLE IF EXISTS tweet", [])?;
                tx.execute("DROP TABLE IF EXISTS user", [])?;
                tx.execute("DROP TABLE IF EXISTS file", [])?;
                tx.execute("DROP TABLE IF EXISTS tweet_file", [])?;
                let schema = Self::load_schema()?;
                tx.execute_batch(&schema)?;
                tx.commit()?;
            }
        } else {
            let schema = Self::load_schema()?;
            connection.execute_batch(&schema)?;
        }

        Ok(TweetStore {
            connection: RwLock::new(connection),
        })
    }

    pub async fn check_digest(&self, digest: &str) -> TweetStoreResult<Option<i64>> {
        let connection = self.connection.read().await;
        let mut select = connection.prepare_cached(FILE_SELECT)?;

        Ok(select
            .query_row(params![digest], |row| row.get(0))
            .optional()?)
    }

    pub async fn add_tweets(
        &self,
        digest: &str,
        primary_twitter_id: Option<u64>,
        tweets: &[BrowserTweet],
    ) -> TweetStoreResult<()> {
        let mut connection = self.connection.write().await;
        let mut tx = connection.transaction()?;
        tx.set_drop_behavior(DropBehavior::Commit);

        let mut insert_file = tx.prepare_cached(FILE_INSERT)?;
        insert_file.execute(params![digest, primary_twitter_id.map(SQLiteId)])?;
        let file_id = tx.last_insert_rowid();

        let mut select_tweet = tx.prepare_cached(TWEET_SELECT_FULL)?;
        let mut insert_tweet = tx.prepare_cached(TWEET_INSERT)?;
        let mut insert_tweet_file = tx.prepare_cached(TWEET_FILE_INSERT)?;

        for tweet in tweets {
            let user_id = Self::add_user(
                &tx,
                tweet.user_id,
                &tweet.user_screen_name,
                &tweet.user_name,
            )?;

            let existing_id: Option<i64> = select_tweet
                .query_row(
                    params![
                        SQLiteId(tweet.id),
                        SQLiteId(tweet.parent_id.unwrap_or(tweet.id)),
                        SQLiteDateTime(tweet.time),
                        SQLiteId(tweet.user_id),
                        tweet.text
                    ],
                    |row| row.get(0),
                )
                .optional()?;

            let tweet_id = match existing_id {
                None => {
                    insert_tweet.execute(params![
                        SQLiteId(tweet.id),
                        SQLiteId(tweet.parent_id.unwrap_or(tweet.id)),
                        SQLiteDateTime(tweet.time),
                        SQLiteId(tweet.user_id),
                        tweet.text
                    ])?;

                    tx.last_insert_rowid()
                }
                Some(id) => id,
            };

            insert_tweet_file.execute(params![tweet_id, file_id, user_id])?;
        }

        Ok(())
    }

    fn load_schema() -> std::io::Result<String> {
        std::fs::read_to_string("schemas/tweet.sql")
    }

    fn add_user(
        tx: &Transaction,
        twitter_id: u64,
        screen_name: &str,
        name: &str,
    ) -> TweetStoreResult<i64> {
        let mut select = tx.prepare_cached(USER_SELECT)?;
        let id = match select
            .query_row(params![SQLiteId(twitter_id), screen_name, name], |row| {
                row.get(0)
            })
            .optional()?
        {
            Some(id) => id,
            None => {
                let mut insert = tx.prepare_cached(USER_INSERT)?;
                insert.execute(params![SQLiteId(twitter_id), screen_name, name])?;
                tx.last_insert_rowid()
            }
        };
        Ok(id)
    }

    pub async fn get_tweet(
        &self,
        status_ids: &[u64],
    ) -> TweetStoreResult<Vec<(BrowserTweet, String)>> {
        let connection = self.connection.read().await;
        let mut select = connection.prepare_cached(TWEET_SELECT_BY_ID)?;
        let mut result = Vec::with_capacity(status_ids.len());

        for id in status_ids {
            match select.query_row(params![SQLiteId(*id)], |row| {
                let parent_twitter_id = row.get::<usize, i64>(0)? as u64;
                let ts: SQLiteDateTime = row.get(1)?;
                let user_twitter_id = row.get::<usize, i64>(2)? as u64;
                let screen_name: String = row.get(3)?;
                let name: String = row.get(4)?;
                let content: String = row.get(5)?;
                let digest: String = row.get(6)?;

                Ok((
                    BrowserTweet::new(
                        *id,
                        if parent_twitter_id == *id {
                            None
                        } else {
                            Some(parent_twitter_id)
                        },
                        ts.0,
                        user_twitter_id,
                        screen_name,
                        name,
                        content,
                    ),
                    digest,
                ))
            }) {
                Ok(pair) => result.push(pair),
                Err(error) => log::error!("Error for {}: {:?}", id, error),
            }
        }

        Ok(result)
    }
}
