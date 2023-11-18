## Usage

1. [Generate a Github token here](https://github.com/settings/tokens/new) with scope `read:org`. A token is required for increased request limits and the `read:org` is required to get the member list. Add the token to your environment:

    ```bash
    export GITHUB_TOKEN=ghp_abc123
    ```

2. Fetch the data from Github

    ```bash
    cargo run -- fetch lichess-org
    ```

3. Optional: Cleanup the data

    ```
    sqlite3 database.sqlite

    delete from pull_requests where username in ('dependabot[bot]', 'dependabot-preview[bot]', 'scala-steward');
    delete from pull_requests where title like 'New Crowdin %';
    ```

4. Format the data

    ```bash
    cargo run -- results
    ```

5. View the report

    ```bash
    cargo run -- serve 9001
    ```

   Visit http://localhost:9001

## Other Queries

```
sqlite3 database.sqlite

.tables
select count(*) from pull_requests;
select * from pull_requests order by created_at asc limit 10;
select count(distinct username) from pull_requests where created_at >= '2023';
```
