use actix_files as fs;
use async_openai::{
    types::{ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs},
    Client,
};
use std::{env, fs::File, io::Write};

use actix_web::{App, HttpServer};
use octocrab::{
    models::{pulls::PullRequest, Author, Repository},
    params, OctocrabBuilder,
};
use rusqlite::Connection;
use serde::Serialize;

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
            fetch(org).await.unwrap();
        }
        "results" => {
            results_first_time_contributions(&args[2], &args[3], 5).unwrap(); // yearly
            results_first_time_contributions(&args[2], &args[3], 8).unwrap(); // monthly
            results_pull_requests(&args[2], &args[3]).unwrap();
        }
        "changelog" => {
            list_merged_pull_requests(&args[2], &args[3], true).unwrap();
            list_merged_pull_requests(&args[2], &args[3], false).unwrap();
        }
        "summary" => {
            summarize(&args[2], &args[3], &args[4]).await.unwrap();
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

async fn fetch(org: &str) -> octocrab::Result<()> {
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

    let token = env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN environment variable not set");

    let octocrab = OctocrabBuilder::new().personal_token(token).build()?;

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
            SELECT username, substr(MIN(created_at), 0, {length}) AS first_date
                FROM pull_requests pr
                WHERE
                    pr.created_at >= '{since}' AND pr.created_at <= '{until}'
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
            substr(pr.created_at, 0, 8) AS month,
            COUNT(CASE WHEN m.username IS NOT NULL THEN 1 END) AS count_pull_requests_by_members,
            COUNT(CASE WHEN m.username IS NULL THEN 1 END) AS count_pull_requests_by_non_members
        FROM
            pull_requests pr
        LEFT JOIN
            members m ON pr.username = m.username
        WHERE
            pr.created_at >= '{since}' AND pr.created_at <= '{until}'
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
        changelog.push_str(&format!(
            "* {title} [#{pr_num}](https://github.com/lichess-org/{repo}/pull/{pr_num}) (thanks [{username}](https://github.com/{username}))\n",
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
            pr.created_at >= ?1 AND pr.created_at <= ?2;;
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
            pr.created_at >= ?1 AND pr.created_at <= ?2
            AND username NOT IN
                (SELECT distinct username FROM pull_requests WHERE created_at < ?1);
        ",
    )?;
    let mut rows = stmt.query([since, until])?;
    let row: &rusqlite::Row<'_> = rows.next().unwrap().unwrap();
    let new_contributors: i32 = row.get(0)?;

    let summary = format!(
        "```
        {org_name} Github Summary for {since} to {until}
        ---------------------------------------------------

        Total pull requests: {count_pull_requests}
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

    println!("**Tweet ideas:**");
    openai_prompt(&format!(
        "Write 5 ideas for Twitter posts about this summary. Do not use emojis or hash tags. {summary}"
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
