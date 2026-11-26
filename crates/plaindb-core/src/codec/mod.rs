mod json;
mod md;
mod yaml;

use serde_json::{Map, Value};

use crate::config::Format;
use crate::error::Result;

pub use json::JsonCodec;
pub use md::MdCodec;
pub use yaml::YamlCodec;

pub type Doc = Map<String, Value>;

pub trait Codec {
    fn decode(&self, bytes: &[u8]) -> Result<Doc>;
    fn encode(&self, doc: &Doc) -> Result<Vec<u8>>;
}

pub fn codec_for(format: Format) -> &'static dyn Codec {
    match format {
        Format::Json => &JsonCodec,
        Format::Yaml => &YamlCodec,
        Format::Md => &MdCodec,
    }
}
