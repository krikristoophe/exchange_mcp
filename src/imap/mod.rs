mod parse;
pub mod client;

pub use client::ImapClient;
pub use parse::html_to_text;
pub use parse::strip_quoted_replies;
