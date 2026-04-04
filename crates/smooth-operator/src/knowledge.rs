use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Trait for pluggable RAG knowledge base backends.
pub trait KnowledgeBase: Send + Sync {
    /// Ingest a document into the knowledge base.
    ///
    /// # Errors
    /// Returns error if the ingestion backend fails.
    fn ingest(&self, doc: Document) -> anyhow::Result<()>;

    /// Query the knowledge base, returning up to `limit` relevant chunks.
    ///
    /// # Errors
    /// Returns error if the query backend fails.
    fn query(&self, query: &str, limit: usize) -> anyhow::Result<Vec<KnowledgeResult>>;
}

/// A document to be ingested into the knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub content: String,
    pub source: String,
    pub doc_type: DocumentType,
    pub metadata: HashMap<String, String>,
}

impl Document {
    /// Create a new document with the given content, source, and type.
    pub fn new(content: impl Into<String>, source: impl Into<String>, doc_type: DocumentType) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            content: content.into(),
            source: source.into(),
            doc_type,
            metadata: HashMap::new(),
        }
    }

    /// Add metadata key-value pair (builder pattern).
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Classification of documents in the knowledge base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentType {
    Code,
    Markdown,
    Config,
    Documentation,
    Conversation,
}

/// A single result from a knowledge base query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeResult {
    pub document_id: String,
    pub chunk: String,
    pub score: f32,
    pub source: String,
}

/// A stored chunk with its parent document reference.
#[derive(Debug, Clone)]
struct StoredChunk {
    document_id: String,
    source: String,
    chunk: String,
}

/// In-memory implementation of the `KnowledgeBase` trait.
///
/// Documents are chunked on ingest (split by double newline, max 500 chars).
/// Query performs TF-IDF-like keyword scoring: for each chunk, count matching
/// query words divided by total words in the chunk.
pub struct InMemoryKnowledge {
    chunks: Mutex<Vec<StoredChunk>>,
}

impl InMemoryKnowledge {
    pub fn new() -> Self {
        Self {
            chunks: Mutex::new(Vec::new()),
        }
    }

    /// Split content into chunks: split on double newlines, then enforce a max
    /// character limit per chunk. Chunks exceeding the limit are split at word
    /// boundaries.
    fn chunk_content(content: &str) -> Vec<String> {
        const MAX_CHUNK_CHARS: usize = 500;

        let sections: Vec<&str> = content.split("\n\n").collect();
        let mut chunks = Vec::new();

        for section in sections {
            let trimmed = section.trim();
            if trimmed.is_empty() {
                continue;
            }

            if trimmed.len() <= MAX_CHUNK_CHARS {
                chunks.push(trimmed.to_string());
            } else {
                // Split at word boundaries
                let mut current = String::new();
                for word in trimmed.split_whitespace() {
                    if current.is_empty() {
                        current.push_str(word);
                    } else if current.len() + 1 + word.len() > MAX_CHUNK_CHARS {
                        chunks.push(current);
                        current = word.to_string();
                    } else {
                        current.push(' ');
                        current.push_str(word);
                    }
                }
                if !current.is_empty() {
                    chunks.push(current);
                }
            }
        }

        chunks
    }
}

impl Default for InMemoryKnowledge {
    fn default() -> Self {
        Self::new()
    }
}

impl KnowledgeBase for InMemoryKnowledge {
    fn ingest(&self, doc: Document) -> anyhow::Result<()> {
        let mut store = self.chunks.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        let chunks = Self::chunk_content(&doc.content);

        for chunk in chunks {
            store.push(StoredChunk {
                document_id: doc.id.clone(),
                source: doc.source.clone(),
                chunk,
            });
        }

        Ok(())
    }

    fn query(&self, query: &str, limit: usize) -> anyhow::Result<Vec<KnowledgeResult>> {
        let store = self.chunks.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        let query_words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

        if query_words.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<KnowledgeResult> = store
            .iter()
            .filter_map(|stored| {
                let chunk_lower = stored.chunk.to_lowercase();
                let chunk_words: Vec<&str> = chunk_lower.split_whitespace().collect();
                if chunk_words.is_empty() {
                    return None;
                }

                let matching = query_words.iter().filter(|qw| chunk_words.iter().any(|cw| cw.contains(qw.as_str()))).count();

                if matching > 0 {
                    #[allow(clippy::cast_precision_loss)]
                    let relevance = matching as f32 / chunk_words.len() as f32;
                    Some(KnowledgeResult {
                        document_id: stored.document_id.clone(),
                        chunk: stored.chunk.clone(),
                        score: relevance,
                        source: stored.source.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_creation_and_serialization() {
        let doc = Document::new("fn main() {}", "src/main.rs", DocumentType::Code).with_metadata("language", "rust");

        assert_eq!(doc.content, "fn main() {}");
        assert_eq!(doc.source, "src/main.rs");
        assert_eq!(doc.doc_type, DocumentType::Code);
        assert_eq!(doc.metadata.get("language"), Some(&"rust".to_string()));

        let json = serde_json::to_string(&doc).expect("serialize");
        let parsed: Document = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.content, "fn main() {}");
        assert_eq!(parsed.source, "src/main.rs");
        assert_eq!(parsed.doc_type, DocumentType::Code);
    }

    #[test]
    fn in_memory_ingest_and_query() {
        let kb = InMemoryKnowledge::new();
        kb.ingest(Document::new(
            "Rust is a systems programming language focused on safety",
            "docs/intro.md",
            DocumentType::Documentation,
        ))
        .expect("ingest");

        let results = kb.query("rust safety", 10).expect("query");
        assert!(!results.is_empty());
        assert!(results[0].score > 0.0);
        assert_eq!(results[0].source, "docs/intro.md");
    }

    #[test]
    fn document_chunking_splits_on_double_newlines() {
        let content = "First section about Rust.\n\nSecond section about Python.\n\nThird section about Go.";
        let chunks = InMemoryKnowledge::chunk_content(content);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "First section about Rust.");
        assert_eq!(chunks[1], "Second section about Python.");
        assert_eq!(chunks[2], "Third section about Go.");
    }

    #[test]
    fn query_returns_scored_results_sorted_by_relevance() {
        let kb = InMemoryKnowledge::new();
        kb.ingest(Document::new(
            "rust programming language\n\npython data science",
            "doc1.md",
            DocumentType::Documentation,
        ))
        .expect("ingest");
        kb.ingest(Document::new("rust compiler and rust toolchain", "doc2.md", DocumentType::Documentation))
            .expect("ingest");

        let results = kb.query("rust", 10).expect("query");
        assert!(results.len() >= 2);
        // The chunk with higher density of "rust" should score higher
        assert!(results[0].score >= results[1].score);
    }

    #[test]
    fn query_no_matches_returns_empty() {
        let kb = InMemoryKnowledge::new();
        kb.ingest(Document::new("rust programming", "doc.md", DocumentType::Documentation))
            .expect("ingest");

        let results = kb.query("javascript", 10).expect("query");
        assert!(results.is_empty());
    }

    #[test]
    fn document_type_variants_serialize_correctly() {
        let types = [
            DocumentType::Code,
            DocumentType::Markdown,
            DocumentType::Config,
            DocumentType::Documentation,
            DocumentType::Conversation,
        ];

        for dt in &types {
            let json = serde_json::to_string(dt).expect("serialize");
            let parsed: DocumentType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*dt, parsed);
        }

        assert!(serde_json::to_string(&DocumentType::Code).expect("serialize").contains("Code"));
        assert!(serde_json::to_string(&DocumentType::Markdown).expect("serialize").contains("Markdown"));
        assert!(serde_json::to_string(&DocumentType::Config).expect("serialize").contains("Config"));
        assert!(serde_json::to_string(&DocumentType::Documentation)
            .expect("serialize")
            .contains("Documentation"));
        assert!(serde_json::to_string(&DocumentType::Conversation).expect("serialize").contains("Conversation"));
    }
}
