//! Optional local BPE encodings. No model-name guessing or runtime vocabulary downloads.
use crate::TokenizerMode;
use aicoder_core::tokens::*;
use std::sync::Arc;
pub fn counter(mode: TokenizerMode) -> Arc<dyn TokenCounter> {
    match mode {
        TokenizerMode::Heuristic => Arc::new(HeuristicTokenCounter),
        mode => {
            let encoding = match mode {
                TokenizerMode::Cl100kBase => Some("cl100k_base".into()),
                TokenizerMode::O200kBase => Some("o200k_base".into()),
                _ => None,
            };
            Arc::new(TokenizedCounter::new(Arc::new(LocalTokenizer), encoding))
        }
    }
}
/// Encodes ordinary text literally, including strings resembling special tokens.
/// Request framing and hidden reasoning are not certified by a text tokenizer.
pub struct LocalTokenizer;
impl TextTokenizer for LocalTokenizer {
    fn count_text(&self, text: &str, encoding: &str) -> Result<u64, TokenFallback> {
        if text.len() > 2 * 1024 * 1024 {
            return Err(TokenFallback::InputLimit);
        }
        #[cfg(feature = "tokenizer-tiktoken")]
        {
            let bpe = match encoding {
                "cl100k_base" => tiktoken_rs::cl100k_base_singleton(),
                "o200k_base" => tiktoken_rs::o200k_base_singleton(),
                _ => return Err(TokenFallback::UnsupportedTokenizer),
            };
            Ok(bpe.count_ordinary(text) as u64)
        }
        #[cfg(not(feature = "tokenizer-tiktoken"))]
        {
            let _ = (text, encoding);
            Err(TokenFallback::TokenizerUnavailable)
        }
    }
}
