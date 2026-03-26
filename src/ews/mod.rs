//! Exchange Web Services (EWS) client for calendar operations.
//!
//! Exchange does not expose calendar items via IMAP (IPM.Appointment items
//! cannot be retrieved through IMAP4). EWS provides SOAP/XML access to
//! calendar data using the same username/password credentials.

pub mod client;
mod xml;

pub use client::EwsClient;
