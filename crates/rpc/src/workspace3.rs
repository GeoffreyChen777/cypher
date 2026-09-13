//! Application RPC over WorkspaceHub. Fragment boundaries are independent of
//! UTF-8 characters, and no partial JSON value reaches an application service.
pub mod codec;
mod host;
pub use host::Host;
mod caller;
pub use caller::{Caller, Responses};
mod links;
pub use links::Links;
