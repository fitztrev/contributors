## Usage

1. [Generate a Github token here](https://github.com/settings/tokens/new) with scope `read:org`. A token is required for increased request limits and the `read:org` is required to get the member list. Add the token to your environment:

    ```bash
    export GITHUB_TOKEN=ghp_abc123
    export OPENAI_API_KEY=sk-abc123 # optional
    ```

2. Reset data

    ```bash
    rm -f database.sqlite
    ```

3. Fetch the data from Github

    ```bash
    cargo run -- fetch lichess-org 2024-12-01 2024-12-31
    ```

4. Optional: Cleanup the data

    ```
    sqlite3 database.sqlite

    delete from pull_requests where username in ('dependabot[bot]', 'dependabot-preview[bot]', 'scala-steward');
    delete from pull_requests where title like 'New Crowdin %';

    insert into members (username) values ('user1'), ('user2');
    ```

    Or add a `queries.sql` file:

    ```bash
    sqlite3 database.sqlite < queries.sql
    ```

5. Format the data

    ```bash
    cargo run -- changelog 2024-12-01 2024-12-31
    cargo run -- summary "Lichess" 2024-12-01 2024-12-31
    ```

6. View the report

    ```bash
    cargo run -- results 2024-12-01 2024-12-31
    cargo run -- serve 9001
    ```

    Visit http://localhost:9001

## Other Queries

```bash
sqlite3 database.sqlite

.tables
```

Total contributors:

```sql
select count(distinct username) from pull_requests;
```

```sql
select count(*) from pull_requests;
select * from pull_requests order by created_at asc limit 10;

select count(distinct username) from pull_requests where created_at >= '2024-01-01' and created_at <= '2024-01-31';
select count(*) from pull_requests where created_at >= '2024-01-01' and created_at <= '2024-01-31';
```
