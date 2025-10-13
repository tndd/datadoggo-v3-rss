# datadoggo-v3-rss
rssフィードを元に情報を集め、dbへの保存を行う。

dbはpsqlを使用する。
- production databaseは`datadoggo_v3`
- test databaseは`test_datadoggo_v3`
- schema名は`rss`

# テーブル定義
Option指定なき場合、NOT NULL制約とする。

## queue
HTTPリクエストの進捗管理テーブル。
linkフィールドについては、blueskyのような例外があるので注意。(例えばblueskyはdescriptionにlinkがあるという方式になってる。)

| name        | type        | description                        |
| ----------- | ----------- | ---------------------------------- |
| id          | uuid(PK)    | アプリケーション側で生成するUUID      |
| created_at  | timestampz  | 作成日時が入る                     |
| updated_at  | timestampz  | 最終更新日時                       |
| link        | text        | rssフィールドのlink                |
| title       | text        | rssフィールドのtitle               |
| pub_date    | timestampz? | rssフィールドのpub_date（未提供時はNULL） |
| description | text        | rssのdescriptionフィールド         |
| status_code | int?        | HTTPステータスコード（未取得時はNULL） |
| group       | text?       | グループ名。分類不要ならNULL       |

## article_content
rssから取得してきた記事データ。
status_codeが200の場合しかここにデータは保存されない。

| name       | type       | description |
| ---------- | ---------- | ----------- |
| queue_id   | uuid       | queueのid   |
| created_at | timestampz | ---         |
| updated_at | timestampz | ---         |
| data       | bytes      | Brotli形式（PostgreSQLではBYTEA） |

# yaml

## rss_links
定義された RSS フィードのエントリ。
便宜上テーブル形式で内容を表示する。

| name  | type     | description |
| ----- | -------- | ----------- |
| url   | text(PK) | URL         |
| group | text     | グループ名  |
| name  | text     | リンク名    |

> **注記**: 設計上は`wait_for_selector`や`timeout`など追加パラメータを受け取れるが、現行バージョンでは未対応のため`rss_links.yml`に指定しても処理では利用されない。

# ドメインモデル

## Article
queueにarticle_contentをjoinしたもの。

| name        | type        | description                |
| ----------- | ----------- | -------------------------- |
| id          | uuid(PK)    | queueのidが入ることになる  |
| created_at  | timestampz  | queueの生成日時            |
| updated_at  | timestampz  | queueの最終更新日時        |
| link        | text        | rssフィールドのlink        |
| title       | text        | rssフィールドのtitle       |
| pub_date    | timestampz? | rssフィールドのpub_date    |
| description | text        | rssのdescriptionフィールド |
| data        | bytes       | Brotli圧縮済み記事のバイナリ |
| group       | text?       | グループ名                 |

アプリケーションでは `Article` 構造体および `search_articles` を提供し、最新の記事をqueueとarticle_contentの結合結果として取得できる。

# api
記事取得は以下のapiで行う。

## 1. ヘルスチェック
```bash
curl -s http://localhost:8000/health | jq
```
レスポンス例:
```json
{
  "status": "ok"
}
```

## 2. スクレイピングAPI
```bash
curl -s -X POST http://localhost:8000/fetch \
  -H "Content-Type: application/json" \
  -d '{
        "url": "https://example.com",
        "wait_for_selector": "article",
        "timeout": 15
      }' | jq
```

レスポンス例（抜粋）:
```json
{
  "html": "<!DOCTYPE html>...",
  "status_code": 200,
  "title": "Example Domain",
  "final_url": "https://example.com/",
  "elapsed_ms": 2387.1,
  "timestamp": "2025-10-09T08:20:33.123456Z"
}
```
- `wait_for_selector` は指定したCSSセレクタが描画されるまで待機します。不要であれば省略できます。
- `timeout` はページロードおよび待機の上限秒数です。
- レスポンスの `html` は取得したDOM全体、`elapsed_ms` は処理時間(ミリ秒)を示します。

# 内部API

CLIに加えてHTTPインターフェースを提供し、外部サービスから処理を呼び出せるようにしている。

- `GET /health` : サーバの稼働確認用エンドポイント。
- `POST /api/fetch-rss` : RSS巡回を実行し、トータル件数とフィードごとの処理状況をJSONで返す。
- `POST /api/fetch-content` : queue内の`status_code`がNULLまたは200以外のレコードを対象に再取得し、保存件数/エラー件数などをJSONで返す。リクエストボディで`{"limit":100}`など処理件数を指定できる。
