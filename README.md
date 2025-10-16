# datadoggo-v3-rss

RSSフィードから記事を収集してPostgreSQLに保存するCLIツール。

## 機能

- RSSフィードからメタデータ（タイトル、リンク、説明など）を取得
- スクレイピングAPIを使用して記事本文を取得（Bot対策をすり抜ける）
- 取得した記事をBrotli圧縮してPostgreSQLに保存
- 重複チェック機能（linkベースで自動的にUPSERT）

## セットアップ

### 1. 依存関係のインストール

```bash
cargo build
```

### 2. データベース設定

Docker で PostgreSQL を起動：

```bash
# 既存のコンテナを削除（存在する場合）
docker rm -f postgres-docker

# PostgreSQLコンテナを起動
docker run -d \
  --name postgres-docker \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_USER=postgres \
  -p 5432:5432 \
  postgres:16-alpine

# test/stg/prod の3つのDBを作成
docker exec -it postgres-docker psql -U postgres -c "CREATE DATABASE datadoggo_v3_test;"
docker exec -it postgres-docker psql -U postgres -c "CREATE DATABASE datadoggo_v3_stg;"
docker exec -it postgres-docker psql -U postgres -c "CREATE DATABASE datadoggo_v3_prod;"
```

`.env`ファイルを編集してDB接続情報を設定（`.env.example`をコピー）：

```bash
cp .env.example .env
```

環境変数の説明：
- `ENVIRONMENT`: 使用する環境を指定 (`TEST`/`STG`/`PROD`)
  - デフォルトは `TEST`（テスト環境）
  - 環境に応じて自動的に適切なDB URLが選択される
- `DATABASE_URL_TEST`: テスト環境のDB接続情報
- `DATABASE_URL_STG`: ステージング環境のDB接続情報
- `DATABASE_URL_PROD`: 本番環境のDB接続情報
- `DATABASE_URL`: 直接指定する場合に使用（最優先、通常は空でOK）
- `PROD_CONFIRMED`: 本番環境への安全装置（`ENVIRONMENT=PROD`の場合、`true`に設定が必要）

環境の切り替え例：
```bash
# テスト環境で実行（デフォルト）
cargo run -- fetch-rss

# ステージング環境で実行
ENVIRONMENT=STG cargo run -- fetch-rss

# 本番環境で実行（PROD_CONFIRMEDも必要）
ENVIRONMENT=PROD PROD_CONFIRMED=true cargo run -- fetch-rss
```

### 3. マイグレーション実行

ステージング環境用にマイグレーション実行：

```bash
# schema作成
docker exec -i postgres-docker psql -U postgres -d datadoggo_v3_stg < migrations/202510130001_create_schema.sql

# queueテーブル作成
docker exec -i postgres-docker psql -U postgres -d datadoggo_v3_stg < migrations/202510130002_create_queue_table.sql

# article_contentテーブル作成
docker exec -i postgres-docker psql -U postgres -d datadoggo_v3_stg < migrations/202510130003_create_article_content_table.sql
```

テスト環境用にも同様に実行：

```bash
docker exec -i postgres-docker psql -U postgres -d datadoggo_v3_test < migrations/202510130001_create_schema.sql
docker exec -i postgres-docker psql -U postgres -d datadoggo_v3_test < migrations/202510130002_create_queue_table.sql
docker exec -i postgres-docker psql -U postgres -d datadoggo_v3_test < migrations/202510130003_create_article_content_table.sql
```

### 4. RSSフィード設定

`rss_links.yml`を編集して監視したいRSSフィードを追加。

## 使い方

### RSSフィードから記事を取得してqueueに登録

```bash
cargo run -- fetch-rss
```

- `rss_links.yml`から対象フィードを読み込み
- 各RSSフィードを取得してパース
- queueテーブルに保存（既存レコードは更新）

### queue内の記事本文を取得

```bash
cargo run -- fetch-content

# 処理件数を指定
cargo run -- fetch-content --limit 50
```

- `status_code=NULL`または`status_code<>200`のエントリを取得
- スクレイピングAPI（現在はモック）を呼び出し
- `status_code=200`の場合のみ記事本文をBrotli圧縮してarticle_contentに保存
- 上記以外のステータスはqueueに記録し直す（再試行可）
- 処理サマリは設定済みのWebhook URLへPOSTされる

### APIサーバを起動

```bash
cargo run -- serve --host 127.0.0.1 --port 8080
```

- `GET /health` : ヘルスチェック（`{"status":"ok"}`を返す）
- `POST /api/fetch-rss` : RSS巡回を実行し、処理結果をJSONで返す
- `POST /api/fetch-content` : queue内の未取得/失敗レコードを再試行する
  - リクエストボディ例: `{"limit": 100}`（省略時は100件）
- `GET /api/articles` : 取得済み記事を新しい順に返す
  - クエリパラメータ `limit`（任意、上限500）と `page_token`（前ページの`next_token`）を受け取る
  - レスポンスは `{ "items": [...], "next_token": "..." }`
  - `items[].content_brotli_base64` にBrotli圧縮本文をBase64エンコードした文字列を格納（総レスポンスは約50MBで打ち切り）
  - 無効な `page_token` を指定した場合は `{"code":"page_token_not_found","message":"page_token is not exist"}` を返す
- 環境変数`WEBHOOK_URL`を設定している場合、各エンドポイント実行後にサマリをWebhookへ送信

## テーブル構成

### rss.queue

| カラム      | 型          | 説明                                             |
| ----------- | ----------- | ------------------------------------------------ |
| id          | UUID        | 主キー（アプリケーション側で生成）               |
| created_at  | TIMESTAMPTZ | 作成日時                                         |
| updated_at  | TIMESTAMPTZ | 更新日時（自動更新）                             |
| link        | TEXT        | 記事URL（UNIQUE制約）                            |
| title       | TEXT        | 記事タイトル                                     |
| pub_date    | TIMESTAMPTZ | 公開日時（NULL許容）                             |
| description | TEXT        | 記事説明                                         |
| status_code | INTEGER     | HTTPステータスコード（NULL許容、未取得時はNULL） |
| group       | TEXT        | グループ名（NULL許容）                           |

### rss.article_content

| カラム     | 型          | 説明                           |
| ---------- | ----------- | ------------------------------ |
| queue_id   | UUID        | 主キー（queue.idへの外部キー） |
| created_at | TIMESTAMPTZ | 作成日時                       |
| updated_at | TIMESTAMPTZ | 更新日時（自動更新）           |
| data       | BYTEA       | Brotli圧縮された記事本文       |

## 開発

### テスト実行

セットアップ手順でDockerコンテナと環境変数を設定済みであれば、そのままテストを実行できます：

```bash
cargo test
```

`.env`ファイルの`TEST_DATABASE_URL`が自動的に使用されます。

手動でテスト用環境変数を設定する場合：

```bash
export TEST_DATABASE_URL="postgresql://postgres:postgres@localhost:5432/datadoggo_v3_test"
cargo test
```

### コンパイルチェック

```bash
cargo check
```

## 今後の拡張予定

- バッチ処理化（cron or systemd timer）
- 失敗タスクの再実行機能
- レート制限実装
- 並行処理の最適化
