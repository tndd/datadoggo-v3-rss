-- article_contentテーブルとトリガーを完全に作り直し
DROP TABLE IF EXISTS rss.article_content CASCADE;

CREATE TABLE rss.article_content (
    queue_id UUID PRIMARY KEY REFERENCES rss.queue(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    data BYTEA NOT NULL
);

-- updated_at自動更新トリガー
CREATE TRIGGER update_article_content_updated_at BEFORE UPDATE ON rss.article_content
    FOR EACH ROW EXECUTE FUNCTION rss.update_updated_at_column();
