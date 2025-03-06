#![warn(clippy::pedantic)]

use actix_files as fs;
use actix_web::{App, HttpServer};
use async_openai::{
    types::{ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs},
    Client,
};
use chrono::{DateTime, LocalResult, TimeZone, Utc};
use octocrab::{
    models::{pulls::PullRequest, repos::RepoCommit, Author, Repository},
    params, OctocrabBuilder,
};
use rusqlite::Connection;
use serde::Serialize;
use std::{env, fs::File, io::Write};

#[derive(Debug, Serialize)]
struct MonthlyNewContributors {
    month: String,
    count: i32,
}

#[derive(Debug, Serialize)]
struct MonthlyPullRequests {
    month: String,
    by_members: i32,
    by_non_members: i32,
    total: i32,
}

#[allow(dead_code)]
#[derive(Debug)]
struct MergedPullRequest {
    id: i32,
    repo: String,
    pr_num: i32,
    username: String,
    title: String,
    created_at: String,
    merged_at: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        println!("Please provide a command");
        return;
    }

    match args[1].as_str() {
        "fetch" => {
            let org = args.get(2).expect("Please provide an org");
            fetch(org, &args[3], &args[4]).await.unwrap();
        }
        "results" => {
            // SQL date query is string comparison and is not end-date inclusive
            let until = next_day(&args[3]);
            results_first_time_contributions(&args[2], &until, 5).unwrap(); // yearly
            results_first_time_contributions(&args[2], &until, 8).unwrap(); // monthly
            results_pull_requests(&args[2], &until).unwrap();
            direct_commits().unwrap();
        }
        "changelog" => {
            let until = next_day(&args[3]);
            list_merged_pull_requests(&args[2], &until, true).unwrap();
            list_merged_pull_requests(&args[2], &until, false).unwrap();
        }
        "summary" => {
            summarize(&args[2], &args[3], &next_day(&args[4]))
                .await
                .unwrap();
        }
        "serve" => {
            let port = args
                .get(2)
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(8080);
            serve(port).await.unwrap();
        }
        _ => println!("Invalid command"),
    }
}

#[allow(clippy::too_many_lines)]
async fn fetch(org: &str, since: &str, until: &str) -> octocrab::Result<()> {
    let conn = Connection::open("database.sqlite").unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS members (
            id              INTEGER PRIMARY KEY,
            username        TEXT NOT NULL,
            UNIQUE(username)
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pull_requests (
            id              INTEGER PRIMARY KEY,
            repo            TEXT NOT NULL,
            pr_num          INTEGER NOT NULL,
            username        TEXT NOT NULL,
            title           TEXT NOT NULL,
            created_at      TEXT NOT NULL,
            merged_at       TEXT,
            UNIQUE(repo, pr_num)
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS commits (
            id              INTEGER PRIMARY KEY,
            repo            TEXT NOT NULL,
            sha             TEXT NOT NULL,
            username        TEXT NOT NULL,
            commited_at     TEXT NOT NULL,
            message         TEXT NOT NULL,
            url             TEXT NOT NULL,
            UNIQUE(sha)
        )",
        [],
    )
    .unwrap();

    let token = env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN environment variable not set");

    let octocrab = OctocrabBuilder::new().personal_token(token).build()?;

    let date_since = parse_date(since, false);
    let date_until = parse_date(until, true);

    fetch_commits_for_repo(&octocrab, &conn, org, "lila", date_since, date_until).await?;
    fetch_commits_for_repo(&octocrab, &conn, org, "mobile", date_since, date_until).await?;

    let mut org_members = octocrab
        .orgs(org)
        .list_members()
        .per_page(100)
        .send()
        .await?;

    loop {
        for member in &org_members {
            conn.execute(
                "INSERT or IGNORE INTO members (username) VALUES (?1)",
                [&member.login],
            )
            .unwrap();
        }

        org_members = match octocrab.get_page::<Author>(&org_members.next).await? {
            Some(next_page) => next_page,
            None => break,
        }
    }

    let mut page_repos = octocrab.orgs(org).list_repos().per_page(100).send().await?;

    loop {
        for repo in &page_repos {
            println!("Getting pull requests for: {}", repo.name);

            let per_page: u8 = 100;

            let mut page_prs = octocrab
                .pulls(org, &repo.name)
                .list()
                .state(params::State::All)
                .sort(params::pulls::Sort::Created)
                .direction(params::Direction::Descending)
                .per_page(per_page)
                .send()
                .await?;

            let mut records: u32 = 0;
            loop {
                for pr in &page_prs {
                    let merged_at = match pr.merged_at {
                        Some(merged_at) => merged_at.to_rfc3339(),
                        None => String::new(),
                    };

                    conn.execute(
                        "INSERT or IGNORE INTO pull_requests (repo, pr_num, username, title, created_at, merged_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        [
                            &pr.clone().base.repo.unwrap().name,
                            &pr.number.to_string(),
                            &pr.clone().user.unwrap().login,
                            &pr.clone().title.unwrap(),
                            &pr.clone().created_at.unwrap().to_rfc3339(),
                            &merged_at,
                        ],
                    ).unwrap();

                    records += 1;
                }

                println!("... {records}");

                page_prs = match octocrab.get_page::<PullRequest>(&page_prs.next).await? {
                    Some(next_page) => next_page,
                    None => break,
                };
            }
        }
        page_repos = match octocrab.get_page::<Repository>(&page_repos.next).await? {
            Some(next_page) => next_page,
            None => break,
        }
    }

    Ok(())
}

async fn fetch_commits_for_repo(
    octocrab: &octocrab::Octocrab,
    conn: &rusqlite::Connection,
    org: &str,
    repo: &str,
    date_since: LocalResult<DateTime<Utc>>,
    date_until: LocalResult<DateTime<Utc>>,
) -> octocrab::Result<()> {
    let mut commits = octocrab
        .repos(org, repo)
        .list_commits()
        .per_page(100)
        .since(date_since.unwrap())
        .until(date_until.unwrap())
        .send()
        .await?;

    loop {
        for commit in &commits {
            let username = match commit.author.clone() {
                Some(author) => author.login,
                None => String::from("n/a"),
            };

            conn.execute(
                "INSERT or IGNORE INTO commits (repo, sha, username, commited_at, message, url) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                [
                    repo,
                    &commit.sha,
                    &username,
                    &commit.commit.clone().committer.unwrap().date.unwrap().to_rfc3339(),
                    &commit.commit.message,
                    &commit.html_url,
                ],
            )
            .unwrap();
        }

        commits = match octocrab.get_page::<RepoCommit>(&commits.next).await? {
            Some(next_page) => next_page,
            None => break,
        }
    }

    Ok(())
}

fn parse_date(date_str: &str, until: bool) -> LocalResult<DateTime<Utc>> {
    let parts: Vec<u32> = date_str
        .split('-')
        .map(|p| p.parse().unwrap())
        .collect();

    let (year, month, day) = (parts[0], parts[1], parts[2]);

    let (hour, min, sec) = if until { (23, 59, 59) } else { (0, 0, 0) };

    Utc.with_ymd_and_hms(year as i32, month, day, hour, min, sec)
}

fn next_day(date: &String) -> String {
    let date = parse_date(date, false).unwrap();
    let next_day = date + chrono::TimeDelta::days(1);
    next_day.format("%Y-%m-%d").to_string()
}

fn results_first_time_contributions(
    since: &String,
    until: &String,
    length: u8,
) -> rusqlite::Result<()> {
    let conn = Connection::open("database.sqlite").unwrap();
    let mut stmt = conn.prepare(
        format!(
            "
        SELECT first_date as date, COUNT(username) as count
        from (
            SELECT username, substr(MIN(merged_at), 0, {length}) AS first_date
                FROM pull_requests pr
                WHERE
                    pr.merged_at >= '{since}' AND pr.merged_at <= '{until}'
                GROUP BY username
        )
        GROUP BY date
        ORDER BY date ASC
    "
        )
        .as_str(),
    )?;

    let month_results = stmt.query_map([], |row| {
        Ok(MonthlyNewContributors {
            month: row.get(0)?,
            count: row.get(1)?,
        })
    })?;

    let month_results: Vec<_> = month_results.collect::<rusqlite::Result<_>>()?;

    let filename = format!("web/results_first_time_contributions_{length}.json");
    let mut file = File::create(&filename).unwrap();
    file.write_all(
        serde_json::to_string_pretty(&month_results)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();

    println!("Results written to {filename}");

    Ok(())
}

fn results_pull_requests(since: &String, until: &String) -> rusqlite::Result<()> {
    let conn = Connection::open("database.sqlite").unwrap();
    let mut stmt = conn.prepare(
        format!(
            "
        SELECT
            substr(pr.merged_at, 0, 8) AS month,
            COUNT(CASE WHEN m.username IS NOT NULL THEN 1 END) AS count_pull_requests_by_members,
            COUNT(CASE WHEN m.username IS NULL THEN 1 END) AS count_pull_requests_by_non_members,
            COUNT(*) AS total
        FROM
            pull_requests pr
        LEFT JOIN
            members m ON pr.username = m.username
        WHERE
            pr.merged_at >= '{since}' AND pr.merged_at <= '{until}'
        GROUP BY
            month
        ORDER BY
            month;
    "
        )
        .as_str(),
    )?;

    let month_results = stmt.query_map([], |row| {
        Ok(MonthlyPullRequests {
            month: row.get(0)?,
            by_members: row.get(1)?,
            by_non_members: row.get(2)?,
            total: row.get(3)?,
        })
    })?;

    let month_results: Vec<_> = month_results.collect::<rusqlite::Result<_>>()?;

    let filename = "web/results_pull_requests.json";
    let mut file = File::create(filename).unwrap();
    file.write_all(
        serde_json::to_string_pretty(&month_results)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();

    println!("Results written to {filename}");

    Ok(())
}

fn direct_commits() -> rusqlite::Result<()> {
    let conn = Connection::open("database.sqlite").unwrap();
    let mut stmt = conn.prepare(
        "
        SELECT
            *
        FROM
            commits
        WHERE
            username in ('ornicar', 'veloce', 'lakinwecker', 'fitztrev', 'niklasf', 'lenguyenthanh', 'lukhas', 'isaacl', 'trevorbayless', 'thomas-daniels', 'benediktwerner', 'kraktus', 'fituby', 'schlawg')
            AND message NOT LIKE '%Merge%'
            AND message NOT LIKE '%New Crowdin updates%'
            AND message NOT LIKE '%New translations%'
            AND message NOT LIKE '%golf%'
            AND message NOT LIKE '%tweak%'
            AND message NOT LIKE '%refactor%'
        ;
        ",
    )?;

    let mut commits = String::new();
    for commit in stmt.query_map([], |row| {
        Ok((
            row.get(1)?, // repo
            row.get(2)?, // sha
            row.get(3)?, // username
            row.get(4)?, // commited_at
            row.get(5)?, // message
            row.get(6)?, // url
        ))
    })? {
        let (repo, sha, username, commited_at, message, url): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = commit.unwrap();

        commits.push_str(&format!(
            "-   {commited_at} - {repo} {username} - {message} [{sha}]({url})\n",
            commited_at = &commited_at[0..10],
            message = message.replace('\n', " "),
            sha = &sha[0..7]
        ));
    }

    let filename = "web/changelog_commits.md";
    let mut file = File::create(filename).unwrap();
    file.write_all(commits.as_bytes()).unwrap();

    Ok(())
}

fn list_merged_pull_requests(
    since: &String,
    until: &String,
    by_members: bool,
) -> rusqlite::Result<()> {
    let conn = Connection::open("database.sqlite").unwrap();

    let query = if by_members {
        format!(
            "SELECT *
                FROM pull_requests
                WHERE merged_at >= '{since}' AND merged_at <= '{until}'
                AND username IN (SELECT username FROM members);"
        )
    } else {
        format!(
            "SELECT *
                FROM pull_requests
                WHERE merged_at >= '{since}' AND merged_at <= '{until}'
                AND username NOT IN (SELECT username FROM members);"
        )
    };

    let mut stmt = conn.prepare(query.as_str())?;

    let prs = stmt.query_map([], |row| {
        Ok(MergedPullRequest {
            id: row.get(0)?,
            repo: row.get(1)?,
            pr_num: row.get(2)?,
            username: row.get(3)?,
            title: row.get(4)?,
            created_at: row.get(5)?,
            merged_at: row.get(6)?,
        })
    })?;

    let prs: Vec<_> = prs.collect::<rusqlite::Result<_>>()?;

    // create a Markdown list of PRs
    let mut changelog = String::new();
    for pr in prs {
        let repo_prefix: String = match pr.repo.as_str() {
            "lila" | "lila-ws" | "lifat" => String::new(),
            "api" => String::from("API Docs: "),
            _ => format!("{}: ", capitalize_first_letter(&pr.repo)),
        };

        changelog.push_str(&format!(
            "-   {repo_prefix}{title} [#{pr_num}](https://github.com/lichess-org/{repo}/pull/{pr_num}) (thanks [{username}](https://github.com/{username}))\n",
            title = capitalize_first_letter(&pr.title),
            pr_num = pr.pr_num,
            repo = pr.repo,
            username = pr.username,
        ));
    }

    let filename = if by_members {
        "web/changelog_members.md"
    } else {
        "web/changelog_non_members.md"
    };
    let mut file = File::create(filename).unwrap();
    file.write_all(changelog.as_bytes()).unwrap();

    Ok(())
}

fn capitalize_first_letter(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

async fn summarize(org_name: &String, since: &String, until: &String) -> rusqlite::Result<()> {
    let conn = Connection::open("database.sqlite").unwrap();

    let mut stmt = conn.prepare(
        "
        SELECT
            COUNT(CASE WHEN m.username IS NOT NULL THEN 1 END) AS count_pull_requests_by_members,
            COUNT(CASE WHEN m.username IS NULL THEN 1 END) AS count_pull_requests_by_non_members
        FROM
            pull_requests pr
        LEFT JOIN
            members m ON pr.username = m.username
        WHERE
            pr.merged_at >= ?1 AND pr.merged_at <= ?2;
    ",
    )?;
    let mut rows = stmt.query([since, until])?;
    let row = rows.next().unwrap().unwrap();
    let count_pull_requests_by_members: i32 = row.get(0)?;
    let count_pull_requests_by_non_members: i32 = row.get(1)?;

    let mut stmt = conn.prepare(
        "
        SELECT
            count(*),
            count(distinct username),
            count(distinct repo)
        FROM
            pull_requests pr
        WHERE
            pr.merged_at >= ?1 AND pr.merged_at <= ?2;;
        ",
    )?;
    let mut rows = stmt.query([since, until])?;
    let row = rows.next().unwrap().unwrap();
    let count_pull_requests: i32 = row.get(0)?;
    let count_contributors: i32 = row.get(1)?;
    let count_repos: i32 = row.get(2)?;

    let mut stmt = conn.prepare(
        "
        SELECT
            count(distinct username)
        FROM
            pull_requests pr
        WHERE
            pr.merged_at >= ?1 AND pr.merged_at <= ?2
            AND username NOT IN
                (SELECT distinct username FROM pull_requests WHERE merged_at < ?1);
        ",
    )?;
    let mut rows = stmt.query([since, until])?;
    let row: &rusqlite::Row<'_> = rows.next().unwrap().unwrap();
    let new_contributors: i32 = row.get(0)?;

    let summary = format!(
        "```
        {org_name} Github Summary for {since} to {until}
        ---------------------------------------------------

        Total merged pull requests: {count_pull_requests}
        Total repos with pull requests: {count_repos}
        Total contributors: {count_contributors}
        First time contributors: {new_contributors}

        Merged pull requests (from team members): {count_pull_requests_by_members}
        Merged pull requests (from community members): {count_pull_requests_by_non_members}```"
    );

    println!("{summary}");

    println!("**Summary idea:**");
    openai_prompt(&format!(
        "Write a short paragraph summarizing this data for the intro of a blog post: {summary}"
    ))
    .await
    .unwrap();

    println!("");
    println!("**Tweet ideas:**");
    openai_prompt(&format!(
        "Write 5 ideas for Twitter posts about this summary. Do not use emojis or hash tags. {summary}"
    ))
    .await
    .unwrap();

    println!("");
    println!("**Feed ideas:**");
    openai_prompt(&format!(
        "Write 5 ideas for short blurbs about this summary. Do not use emojis or hash tags. Include a markdown link for reading more to https://lichess.org/changelog : {summary}"
    ))
    .await
    .unwrap();

    Ok(())
}

async fn openai_prompt(prompt: &str) -> Result<(), Box<dyn std::error::Error>> {
    if env::var("OPENAI_API_KEY").is_err() {
        println!("Skipping OpenAI API call until OPENAI_API_KEY is set");
        return Ok(());
    }

    let client = Client::new();

    let request = CreateChatCompletionRequestArgs::default()
        .model("gpt-4")
        .messages([ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()?
            .into()])
        .build()?;

    let response = client.chat().create(request).await?;

    for choice in response.choices {
        let message = choice.message.content.unwrap();

        for line in message.split('\n') {
            println!("{line}");
        }
    }

    Ok(())
}

async fn serve(port: u16) -> std::io::Result<()> {
    println!("Starting server at http://localhost:{port}");

    HttpServer::new(|| App::new().service(fs::Files::new("/", "web").index_file("index.html")))
        .bind(("0.0.0.0", port))?
        .run()
        .await
}
