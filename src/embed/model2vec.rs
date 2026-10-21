//! In-process static embeddings: a token-embedding lookup plus mean pooling. No network
//! after the model files are cached (`~/.cache/huggingface`, or a local folder path).

use super::{EmbedConfig, EmbedError, Embedder, Result, normalize, truncate};

pub struct Model2VecEmbedder {
    model: model2vec_rs::model::StaticModel,
    id: String,
}

impl Model2VecEmbedder {
    pub fn load(cfg: &EmbedConfig) -> Result<Self> {
        let repo_or_path = cfg.model();
        let model = model2vec_rs::model::StaticModel::from_pretrained(&repo_or_path, None, Some(true), None)
            .map_err(|e| EmbedError::Model(e.to_string()))?;
        Ok(Self {
            model,
            id: format!("model2vec:{repo_or_path}"),
        })
    }
}

impl Embedder for Model2VecEmbedder {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = texts.iter().map(|t| truncate(t).to_string()).collect();
        Ok(self.model.encode(&inputs).into_iter().map(normalize).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(normalize(self.model.encode_single(text)))
    }
}
