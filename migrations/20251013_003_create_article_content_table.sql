-- article_contentテーブル作成
CREATE TABLE IF NOT EXISTS rss.article_content (
    queue_id UUID PRIMARY KEY REFERENCES rss.queue(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    data BYTEA NOT NULL
);

-- updated_at自動更新トリガー
CREATE TRIGGER update_article_content_updated_at BEFORE UPDATE ON rss.article_content
    FOR EACH ROW EXECUTE FUNCTION rss.update_updated_at_column();
