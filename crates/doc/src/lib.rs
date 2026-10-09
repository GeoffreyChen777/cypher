//! cypher-doc — session & registry Loro doc schemas and the typed mirror layer.
//!
//! Ported from zeron's session doc. The schema SHAPE (container names, part maps with
//! LoroText bodies, command entries) is kept identical to the TS implementation so the edge's
//! tail materializer and any TS peer remain compatible.
//!
//! Load-bearing invariant (measured in zeron): message parts are a
//! LoroList of part maps whose text bodies live in **LoroText** — streaming appends RLE-merge at
//! ~1.03x oplog overhead, whereas rewriting whole part values costs ~125x.

mod commands;
mod constants;
mod parts;
mod preview;
mod registry;
mod schema;
pub mod transcript_delta;

pub use commands::*;
pub use constants::*;
pub use parts::*;
pub use preview::*;
pub use registry::*;
pub use schema::*;
pub use transcript_delta::*;
